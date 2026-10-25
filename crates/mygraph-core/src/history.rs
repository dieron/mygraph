//! Operation `history` — spec/api/operations/history.md.

use crate::engine::{Graph, OpError, storage_error};
use crate::ids::{ObjectId, ObjectKind};
use crate::model::{id_string, obj_kind_code, time_json};
use crate::store::Store;
use serde_json::{Value as Json, json};

fn bad(field: &str, expected: &str) -> OpError {
    OpError::new(
        "history.invalid_field",
        format!("{field}: expected {expected}"),
    )
}

fn kind_of_code(code: i64) -> Option<ObjectKind> {
    match code {
        0 => Some(ObjectKind::Node),
        1 => Some(ObjectKind::Link),
        2 => Some(ObjectKind::Evidence),
        3 => Some(ObjectKind::Citation),
        _ => None,
    }
}

impl<S: Store> Graph<S> {
    /// Operation `history` ([OP-HIST-1] .. [OP-HIST-3]).
    pub fn history(&self, request: &Json) -> Result<Json, OpError> {
        let req = request
            .as_object()
            .ok_or_else(|| bad("request", "an object"))?;
        let int = |key: &str| -> Result<Option<i64>, OpError> {
            req.get(key)
                .map(|v| {
                    v.as_i64()
                        .filter(|n| *n >= 0)
                        .ok_or_else(|| bad(key, "an integer >= 0"))
                })
                .transpose()
        };
        let since = int("since_seq")?;
        let before = int("before_seq")?;
        let object = match (req.get("target"), since) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(bad("target", "either target or since_seq"));
            }
            (Some(t), None) => Some(self.history_target(t)?),
            (None, Some(_)) => {
                if before.is_some() {
                    return Err(bad("before_seq", "only with target"));
                }
                None
            }
        };
        let default = if object.is_some() { 20 } else { 100 };
        let limit = match req.get("limit") {
            None => default,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=500).contains(n))
                .ok_or_else(|| bad("limit", "an integer from 1 to 500"))?
                as usize,
        };
        let code = object.map(|(k, i)| (obj_kind_code(k), i));
        let mut rows = self
            .store
            .history(code, since, before, limit)
            .map_err(storage_error)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let st = &self.state;
        let entries: Vec<Json> = rows
            .into_iter()
            .map(|r| {
                let object = match (kind_of_code(r.obj_kind), r.obj) {
                    (Some(k), Some(i)) => Json::String(id_string(k, i)),
                    _ => Json::Null,
                };
                let diff = r
                    .diff
                    .as_deref()
                    .and_then(|d| serde_json::from_str::<Json>(d).ok())
                    .unwrap_or(Json::Null);
                json!({
                    "seq": r.seq, "at": time_json(r.ts), "actor": st.actor_name(r.actor),
                    "batch": id_string(ObjectKind::Batch, r.batch), "op": r.op,
                    "object": object, "diff": diff,
                })
            })
            .collect();
        Ok(json!({ "entries": entries, "last_seq": self.counters.seq, "more": more }))
    }

    /// A target reference; a node ID stays on that node even when it was merged ([OP-HIST-1]).
    fn history_target(&self, t: &Json) -> Result<(ObjectKind, i64), OpError> {
        if let Some(s) = t.as_str()
            && let Ok(id) = ObjectId::decode(s)
        {
            let idx = id.number as i64;
            if self.state.get(id.kind, idx).is_some() {
                return Ok((id.kind, idx));
            }
            return Err(OpError::new(
                "ref.not_found",
                format!("nothing matches {s}"),
            ));
        }
        let (kind, idx, _) = self.resolve_any(t)?;
        Ok((kind, idx))
    }
}
