//! Operations `retract` and `merge` — spec/api/operations/{retract,merge}.md,
//! spec/model/objects.md §8–§9.

use crate::engine::{Batch, Graph, OpError, StatusOp};
use crate::ids::{ObjectId, ObjectKind};
use crate::model::{Row, Status, TargetKind, id_string, obj_kind_code};
use crate::state::{CitationIdentity, LinkIdentity};
use crate::store::Store;
use crate::values::values_equal;
use crate::write::{Refs, as_obj, get, opt_str};
use serde_json::{Map, Value as Json, json};

fn kind_from_code(code: i64) -> Option<ObjectKind> {
    match code {
        0 => Some(ObjectKind::Node),
        1 => Some(ObjectKind::Link),
        2 => Some(ObjectKind::Evidence),
        3 => Some(ObjectKind::Citation),
        _ => None,
    }
}

fn target_of(kind: ObjectKind) -> Option<TargetKind> {
    match kind {
        ObjectKind::Node => Some(TargetKind::Node),
        ObjectKind::Link => Some(TargetKind::Link),
        _ => None,
    }
}

/// Counts reported by `merge` ([OP-MERGE-3]): (links, citations).
#[derive(Debug, Default)]
struct MergeCounts {
    moved: (u64, u64),
    combined: (u64, u64),
}

fn status_op(
    op: &'static str,
    reason: Option<String>,
    cause: Option<(ObjectKind, i64)>,
) -> StatusOp {
    StatusOp {
        op,
        reason,
        cause,
        extra: Map::new(),
    }
}

impl<S: Store> Graph<S> {
    /// Operation `retract` ([OP-RETRACT-1] .. [OP-RETRACT-6]).
    pub fn retract(&mut self, actor: &str, request: &Json, now_ms: i64) -> Result<Json, OpError> {
        let req = as_obj(request, "request")?;
        let restore = match opt_str(req, "action")?.as_deref() {
            None | Some("retract") => false,
            Some("restore") => true,
            Some(_) => {
                return Err(OpError::new(
                    "write.invalid_field",
                    "action must be retract or restore",
                ));
            }
        };
        let reason = opt_str(req, "reason")?;
        if reason.as_ref().is_some_and(|r| r.len() > 1024) {
            return Err(OpError::new(
                "write.invalid_field",
                "reason is longer than 1024 bytes",
            ));
        }
        let mut targets = self.retract_targets(req, restore)?;
        if restore {
            // Endpoints and targets first, so restored links and citations find them active.
            targets.sort_by_key(|(k, _)| obj_kind_code(*k));
        }
        let mut b = self.begin(actor, if restore { "restore" } else { "retract" }, now_ms)?;
        let mut items = Vec::new();
        for (i, (kind, idx)) in targets.into_iter().enumerate() {
            let result = if restore {
                self.restore_one(&mut b, kind, idx, reason.clone(), None, &mut items)
            } else {
                self.retract_one(&mut b, kind, idx, reason.clone(), None, &mut items)
            };
            if let Err(e) = result {
                self.rollback(b);
                return Err(e.at("targets", i));
            }
        }
        let (batch, warnings) = self.finish(b, false)?;
        Ok(json!({ "batch": batch, "items": items, "warnings": warnings }))
    }

    fn retract_targets(
        &self,
        req: &crate::write::Obj,
        restore: bool,
    ) -> Result<Vec<(ObjectKind, i64)>, OpError> {
        match (get(req, "targets"), get(req, "batch")) {
            (Some(Json::Array(list)), None) if (1..=1000).contains(&list.len()) => list
                .iter()
                .map(|t| match t {
                    Json::String(s) => {
                        let id = ObjectId::decode(s)?;
                        let kind = id.kind;
                        let idx = id.number as i64;
                        self.state
                            .get(kind, idx)
                            .map(|_| (kind, idx))
                            .ok_or_else(|| {
                                OpError::new("ref.not_found", format!("{s} does not exist"))
                            })
                    }
                    Json::Object(o) => {
                        let kind = o
                            .get("kind")
                            .and_then(Json::as_str)
                            .and_then(ObjectKind::from_name)
                            .ok_or_else(|| {
                                OpError::new("write.invalid_ref", "keyed target needs kind")
                            })?;
                        let idx =
                            self.resolve_ref(&Refs::new(), &json!({ "key": o.get("key") }), kind)?;
                        Ok((kind, idx))
                    }
                    other => Err(OpError::new(
                        "write.invalid_ref",
                        format!("{other} is not a target"),
                    )),
                })
                .collect(),
            (None, Some(Json::String(s))) => {
                let batch = ObjectId::decode_of_kind(s, ObjectKind::Batch)?.number as i64;
                let op = if restore { "retract" } else { "create" };
                let objs = self.store_query(self.store.batch_objects(batch, op))?;
                Ok(objs
                    .into_iter()
                    .filter_map(|(k, i)| kind_from_code(k).map(|k| (k, i)))
                    .collect())
            }
            _ => Err(OpError::new(
                "write.invalid_field",
                "give either targets (1-1000 IDs) or batch",
            )),
        }
    }

    fn item(
        items: &mut Vec<Json>,
        kind: ObjectKind,
        idx: i64,
        outcome: &str,
        cause: Option<(ObjectKind, i64)>,
    ) {
        let mut it = json!({ "id": id_string(kind, idx), "outcome": outcome });
        if let Some((k, i)) = cause {
            it["cascade_of"] = id_string(k, i).into();
        }
        items.push(it);
    }

    fn watch_support(&self, b: &mut Batch, fact: (TargetKind, i64)) {
        let supported = self.state.is_supported(fact.0, fact.1);
        b.support_watch.push((fact, supported));
    }

    pub(crate) fn retract_one(
        &mut self,
        b: &mut Batch,
        kind: ObjectKind,
        idx: i64,
        reason: Option<String>,
        cause: Option<(ObjectKind, i64)>,
        items: &mut Vec<Json>,
    ) -> Result<(), OpError> {
        let row = self.state.get(kind, idx).expect("exists");
        match row.status() {
            Status::Retracted => {
                Self::item(items, kind, idx, "unchanged", cause);
                return Ok(());
            }
            Status::Merged => {
                return Err(OpError::new(
                    "lifecycle.invalid_transition",
                    format!("{} is merged", id_string(kind, idx)),
                ));
            }
            Status::Active => {}
        }
        // Facts whose support may change.
        match &row {
            Row::Citation(c) => self.watch_support(b, (c.target_kind, c.target)),
            Row::Evidence(_) => {
                let cites: Vec<i64> = self
                    .state
                    .cites_by_evidence
                    .get(&idx)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                for c in cites {
                    let c = &self.state.citations[&c];
                    let fact = (c.target_kind, c.target);
                    self.watch_support(b, fact);
                }
            }
            _ => {}
        }
        self.put_status(
            b,
            row,
            Status::Retracted,
            status_op("retract", reason.clone(), cause),
        );
        Self::item(items, kind, idx, "retracted", cause);
        // [OBJ-LIFE-2] [OBJ-LIFE-3] cascades
        let dependents: Vec<(ObjectKind, i64)> = match kind {
            ObjectKind::Node => self
                .state
                .adjacency
                .get(&idx)
                .into_iter()
                .flatten()
                .filter(|l| self.state.links[l].status == Status::Active)
                .map(|&l| (ObjectKind::Link, l))
                .collect(),
            ObjectKind::Evidence => self
                .state
                .cites_by_evidence
                .get(&idx)
                .into_iter()
                .flatten()
                .filter(|c| self.state.citations[c].status == Status::Active)
                .map(|&c| (ObjectKind::Citation, c))
                .collect(),
            _ => Vec::new(),
        };
        for (k, i) in dependents {
            self.retract_one(b, k, i, reason.clone(), Some((kind, idx)), items)?;
        }
        Ok(())
    }

    fn restore_one(
        &mut self,
        b: &mut Batch,
        kind: ObjectKind,
        idx: i64,
        reason: Option<String>,
        cause: Option<(ObjectKind, i64)>,
        items: &mut Vec<Json>,
    ) -> Result<(), OpError> {
        let row = self.state.get(kind, idx).expect("exists");
        match row.status() {
            Status::Active => {
                Self::item(items, kind, idx, "unchanged", cause);
                return Ok(());
            }
            Status::Merged => {
                return Err(OpError::new(
                    "lifecycle.invalid_transition",
                    format!("{} is merged", id_string(kind, idx)),
                ));
            }
            Status::Retracted => {}
        }
        if let Err(e) = self.check_restorable(&row) {
            if cause.is_some() {
                b.warn(
                    "NOT_RESTORED",
                    format!("{} was not restored: {}", id_string(kind, idx), e.message),
                    json!({ "id": id_string(kind, idx), "reason": e.reason.code() }),
                );
                return Ok(());
            }
            return Err(e);
        }
        // Find the cascade of this object's last retraction before changing anything.
        let cascaded = match kind {
            ObjectKind::Node | ObjectKind::Evidence => {
                let last =
                    self.store_query(self.store.last_retraction(obj_kind_code(kind), idx))?;
                match last {
                    Some(seq) => self.store_query(self.store.cascaded_from(seq))?,
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        self.put_status(
            b,
            row,
            Status::Active,
            status_op("restore", reason.clone(), cause),
        );
        Self::item(items, kind, idx, "restored", cause);
        if let Some(t) = target_of(kind) {
            b.new_facts.push((t, idx)); // [OP-RETRACT-5]
        }
        for (k, i) in cascaded {
            let Some(k) = kind_from_code(k) else { continue };
            if self
                .state
                .get(k, i)
                .is_some_and(|r| r.status() == Status::Retracted)
            {
                self.restore_one(b, k, i, reason.clone(), Some((kind, idx)), items)?;
            }
        }
        Ok(())
    }

    /// [OP-RETRACT-4] conditions for bringing an object back.
    fn check_restorable(&self, row: &Row) -> Result<(), OpError> {
        let node_active = |i: &i64| {
            self.state
                .nodes
                .get(i)
                .is_some_and(|n| n.status == Status::Active)
        };
        match row {
            Row::Link(l) => {
                if !node_active(&l.src) || !node_active(&l.dst) {
                    return Err(OpError::new(
                        "link.endpoint_not_active",
                        "an endpoint is not active",
                    ));
                }
                if !l.multi
                    && l.key.is_none()
                    && self
                        .state
                        .find_link(&LinkIdentity::new(&l.type_, l.directed, l.src, l.dst))
                        .is_some()
                {
                    return Err(OpError::new(
                        "lifecycle.identity_taken",
                        "an active link with the same identity exists",
                    ));
                }
            }
            Row::Citation(c) => {
                if self
                    .state
                    .evidence
                    .get(&c.evidence)
                    .is_none_or(|e| e.status != Status::Active)
                {
                    return Err(OpError::new(
                        "citation.evidence_not_active",
                        "its evidence is not active",
                    ));
                }
                let target_active = match c.target_kind {
                    TargetKind::Node => node_active(&c.target),
                    TargetKind::Link => self
                        .state
                        .links
                        .get(&c.target)
                        .is_some_and(|l| l.status == Status::Active),
                };
                if !target_active {
                    return Err(OpError::new(
                        "citation.target_not_active",
                        "its target is not active",
                    ));
                }
                let ident = CitationIdentity {
                    evidence: c.evidence,
                    target_kind: c.target_kind,
                    target: c.target,
                    prop: c.prop.clone(),
                    role: c.role,
                };
                if self.state.find_citation(&ident).is_some() {
                    return Err(OpError::new(
                        "lifecycle.identity_taken",
                        "an active citation with the same identity exists",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Operation `merge` ([OP-MERGE-1] .. [OP-MERGE-3], [OBJ-MERGE-1] .. [OBJ-MERGE-5]).
    pub fn merge(&mut self, actor: &str, request: &Json, now_ms: i64) -> Result<Json, OpError> {
        let req = as_obj(request, "request")?;
        let refs = Refs::new();
        let into = self.resolve_ref(
            &refs,
            get(req, "into").ok_or_else(|| OpError::new("write.missing_field", "missing into"))?,
            ObjectKind::Node,
        )?;
        let list = get(req, "merge")
            .and_then(Json::as_array)
            .filter(|l| (1..=100).contains(&l.len()))
            .ok_or_else(|| {
                OpError::new(
                    "write.invalid_field",
                    "merge must list 1-100 node references",
                )
            })?;
        let prefer_merged = match opt_str(req, "prop_conflicts")?.as_deref() {
            None | Some("keep_into") => false,
            Some("prefer_merged") => true,
            Some(_) => {
                return Err(OpError::new(
                    "write.invalid_field",
                    "prop_conflicts must be keep_into or prefer_merged",
                ));
            }
        };
        let mut losers = Vec::new();
        for r in list {
            let l = self.resolve_ref(&refs, r, ObjectKind::Node)?;
            if l == into {
                return Err(OpError::new(
                    "merge.self",
                    "a node cannot be merged into itself",
                ));
            }
            if !losers.contains(&l) {
                losers.push(l);
            }
        }
        for &n in losers.iter().chain(std::iter::once(&into)) {
            if self.state.nodes[&n].status != Status::Active {
                return Err(OpError::new(
                    "lifecycle.retracted",
                    format!("{} is not active", id_string(ObjectKind::Node, n)),
                ));
            }
        }
        let mut b = self.begin(actor, "merge", now_ms)?;
        let mut counts = MergeCounts::default();
        for &l in &losers {
            if let Err(e) = self.merge_one(&mut b, into, l, prefer_merged, &mut counts) {
                self.rollback(b);
                return Err(e);
            }
        }
        let (batch, warnings) = self.finish(b, false)?;
        Ok(json!({
            "batch": batch,
            "into": id_string(ObjectKind::Node, into),
            "merged": losers.iter().map(|&l| id_string(ObjectKind::Node, l)).collect::<Vec<_>>(),
            "moved": { "links": counts.moved.0, "citations": counts.moved.1 },
            "combined": { "links": counts.combined.0, "citations": counts.combined.1 },
            "warnings": warnings,
        }))
    }

    fn merge_one(
        &mut self,
        b: &mut Batch,
        s: i64,
        l: i64,
        prefer_merged: bool,
        counts: &mut MergeCounts,
    ) -> Result<(), OpError> {
        let survivor_id = id_string(ObjectKind::Node, s);
        let loser = self.state.nodes[&l].clone();
        if loser.type_ != self.state.nodes[&s].type_ {
            b.warn(
                "TYPE_DIFFERS",
                format!("merged node has type {}", loser.type_),
                json!({ "id": id_string(ObjectKind::Node, l), "type": &*loser.type_ }),
            );
        }
        // [OBJ-MERGE-1] the merged node first, so later entries can name it as their cause.
        let mut merged = loser.clone();
        merged.merged_into = Some(s);
        let mut extra = Map::new();
        extra.insert("merged_into".into(), survivor_id.clone().into());
        extra.insert(
            "prop_conflicts".into(),
            if prefer_merged {
                "prefer_merged"
            } else {
                "keep_into"
            }
            .into(),
        );
        self.put_status(
            b,
            Row::Node(merged),
            Status::Merged,
            StatusOp {
                op: "merge",
                reason: None,
                cause: None,
                extra,
            },
        );
        let cause = Some((ObjectKind::Node, l));

        // [OBJ-MERGE-4] survivor fields
        let mut sv = self.state.nodes[&s].clone();
        Self::add_aliases(
            &mut sv,
            std::iter::once(loser.name.clone()).chain(loser.aliases.iter().cloned()),
        )?;
        for lab in &loser.labels {
            if !sv.labels.contains(lab) && sv.labels.len() < 32 {
                sv.labels.push(lab.clone());
            }
        }
        if sv.summary.is_none() {
            sv.summary = loser.summary.clone();
        }
        for (k, v) in &loser.props {
            match sv.props.get(k) {
                None => {
                    sv.props.insert(k.clone(), v.clone());
                }
                Some(old) if values_equal(old, v) => {}
                Some(old) => {
                    let (kept, dropped) = if prefer_merged {
                        (v.clone(), old.clone())
                    } else {
                        (old.clone(), v.clone())
                    };
                    b.warn(
                        "PROPERTY_CONFLICT",
                        format!("{survivor_id}: props.{k} differ"),
                        json!({ "prop": &**k, "kept": kept.to_json(), "dropped": dropped.to_json() }),
                    );
                    sv.props.insert(k.clone(), kept);
                }
            }
        }
        self.put(b, Row::Node(sv));

        // [OBJ-MERGE-2] links
        let links: Vec<i64> = self
            .state
            .adjacency
            .get(&l)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| self.state.links[i].status == Status::Active)
            .collect();
        for li in links {
            let mut link = self.state.links[&li].clone();
            if link.src == l {
                link.src = s;
            }
            if link.dst == l {
                link.dst = s;
            }
            let ident = (!link.multi && link.key.is_none())
                .then(|| LinkIdentity::new(&link.type_, link.directed, link.src, link.dst));
            let duplicate_of = ident
                .and_then(|id| self.state.find_link(&id))
                .filter(|&o| o != li);
            match duplicate_of {
                None => {
                    self.put(b, Row::Link(link));
                    counts.moved.0 += 1;
                }
                Some(keep) => {
                    let mut kept = self.state.links[&keep].clone();
                    for (k, v) in &link.props {
                        if !kept.props.contains_key(k) {
                            kept.props.insert(k.clone(), v.clone());
                        }
                    }
                    self.put(b, Row::Link(kept));
                    self.put_status(
                        b,
                        Row::Link(link),
                        Status::Retracted,
                        status_op("retract", Some("combined by merge".into()), cause),
                    );
                    self.retarget_citations(b, TargetKind::Link, li, keep, cause, counts);
                    counts.combined.0 += 1;
                }
            }
        }
        // [OBJ-MERGE-3] citations of the merged node
        self.retarget_citations(b, TargetKind::Node, l, s, cause, counts);
        Ok(())
    }

    fn retarget_citations(
        &mut self,
        b: &mut Batch,
        kind: TargetKind,
        from: i64,
        to: i64,
        cause: Option<(ObjectKind, i64)>,
        counts: &mut MergeCounts,
    ) {
        let cites: Vec<i64> = self
            .state
            .cites_of(kind, from)
            .into_iter()
            .flatten()
            .copied()
            .filter(|c| self.state.citations[c].status == Status::Active)
            .collect();
        for ci in cites {
            let mut c = self.state.citations[&ci].clone();
            c.target = to;
            let ident = CitationIdentity {
                evidence: c.evidence,
                target_kind: kind,
                target: to,
                prop: c.prop.clone(),
                role: c.role,
            };
            if self.state.find_citation(&ident).is_some_and(|o| o != ci) {
                let mut old = self.state.citations[&ci].clone();
                old.status = Status::Active;
                self.put_status(
                    b,
                    Row::Citation(old),
                    Status::Retracted,
                    status_op("retract", Some("combined by merge".into()), cause),
                );
                counts.combined.1 += 1;
            } else {
                self.put(b, Row::Citation(c));
                counts.moved.1 += 1;
            }
        }
    }
}
