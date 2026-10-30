//! Operation `find` — spec/api/operations/find.md.

use crate::engine::{Graph, OpError};
use crate::ids::ObjectKind;
use crate::model::{Row, Status, TargetKind, id_string, record_json};
use crate::name_key::name_key;
use crate::read::BRIEF;
use crate::store::Store;
use crate::values::{self, DateTime, Parsed, Value, civil_from_days, temporal_bounds};
use crate::vocab::VocabKind;
use serde_json::{Map, Value as Json, json};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

fn bad(field: &str, expected: &str) -> OpError {
    OpError::new(
        "find.invalid_field",
        format!("{field}: expected {expected}"),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Nodes,
    Links,
    Evidence,
    Citations,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Out,
    In,
    Both,
}

fn parse_dir(s: &str) -> Option<Dir> {
    match s.trim() {
        "out" => Some(Dir::Out),
        "in" => Some(Dir::In),
        "both" => Some(Dir::Both),
        _ => None,
    }
}

#[derive(Clone, PartialEq)]
enum Base {
    Id,
    Status,
    CreatedAt,
    UpdatedAt,
    Prop(String),
    EvidenceCount,
    Type,
    Name,
    Labels,
    Degree(Option<String>, Dir),
    From,
    To,
    FromType,
    ToType,
    Directed,
    Kind,
    Title,
    Uri,
    Author,
    Publisher,
    Language,
    SourceDate,
    CapturedAt,
    Citations,
    Evidence,
    CitTarget,
    TargetKind,
    CitProp,
    Role,
    Confidence,
    EvidenceKind,
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Year,
    Month,
    Decade,
    Century,
}

#[derive(Clone, PartialEq)]
enum Expr {
    Field(Base, Option<Part>),
    Bucket(Box<Expr>, f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Exists,
    Missing,
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Between,
    In,
    NotIn,
    Contains,
    StartsWith,
}

enum Cond {
    Cmp(Expr, Op, Option<Value>),
    All(Vec<Cond>),
    Any(Vec<Cond>),
    Not(Box<Cond>),
    Linked(Option<String>, Dir, i64),
}

#[derive(Clone, PartialEq)]
enum AggFn {
    Count,
    CountOf,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
    Median,
}

struct Agg {
    f: AggFn,
    of: Option<Expr>,
    name: String,
}

/// Hashable form of a value for group keys and distinct counts ([VAL-EQ-1]: `1` equals `1.0`).
/// Strings borrow from the in-memory records where they can, so grouping allocates little.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(Cow<'a, str>),
    Date(i32, u8, u8),
    DateTime(i64),
    List(Vec<Key<'a>>),
}

fn key_of(v: Option<&Value>) -> Key<'_> {
    match v {
        None => Key::Null,
        Some(Value::Bool(b)) => Key::Bool(*b),
        Some(Value::Int(i)) => Key::Int(*i),
        Some(Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => Key::Int(*f as i64),
        Some(Value::Float(f)) => Key::Float(f.to_bits()),
        Some(Value::String(s)) => Key::Str(Cow::Borrowed(s)),
        Some(Value::Date(d)) => Key::Date(d.year, d.month, d.day),
        Some(Value::DateTime(t)) => Key::DateTime(t.ms),
        Some(Value::List(l)) => Key::List(l.iter().map(|x| key_of(Some(x))).collect()),
    }
}

fn owned_key(k: Key<'_>) -> Key<'static> {
    match k {
        Key::Null => Key::Null,
        Key::Bool(b) => Key::Bool(b),
        Key::Int(i) => Key::Int(i),
        Key::Float(f) => Key::Float(f),
        Key::Str(s) => Key::Str(Cow::Owned(s.into_owned())),
        Key::Date(y, m, d) => Key::Date(y, m, d),
        Key::DateTime(t) => Key::DateTime(t),
        Key::List(l) => Key::List(l.into_iter().map(owned_key).collect()),
    }
}

/// A finished group: key values, object count, aggregate results by name.
type GroupRow = (Vec<Option<Value>>, u64, Map<String, Json>);

/// Group key: one expression (the usual case) without a vector.
#[derive(PartialEq, Eq, Hash)]
enum GroupKey<'a> {
    One(Key<'a>),
    Many(Vec<Key<'a>>),
}

/// A field value, borrowed from the records when possible.
enum V<'a> {
    Ref(&'a Value),
    Str(&'a str),
    Own(Value),
}

impl<'a> V<'a> {
    fn into_key(self) -> Key<'a> {
        match self {
            V::Ref(v) => key_of(Some(v)),
            V::Str(s) => Key::Str(Cow::Borrowed(s)),
            V::Own(v) => owned_key(key_of(Some(&v))),
        }
    }

    fn value(&self) -> Cow<'_, Value> {
        match self {
            V::Ref(v) => Cow::Borrowed(*v),
            V::Str(s) => Cow::Owned(Value::String((*s).to_owned())),
            V::Own(v) => Cow::Borrowed(v),
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            V::Ref(v) => num(v),
            V::Own(v) => num(v),
            V::Str(_) => None,
        }
    }

    fn is_int(&self) -> Option<i64> {
        match self {
            V::Ref(Value::Int(i)) | V::Own(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }
}

/// [OP-FIND-4] with string fields compared in place.
fn holds_v(v: &V, op: Op, operand: &Value) -> bool {
    match v {
        V::Ref(x) => holds(x, op, operand),
        V::Own(x) => holds(x, op, operand),
        V::Str(s) => match (op, operand) {
            (Op::Eq, Value::String(o)) => *s == o,
            (Op::Ne, Value::String(o)) => *s != o,
            (Op::In, Value::List(xs)) => xs.iter().any(|x| matches!(x, Value::String(o) if o == s)),
            (Op::NotIn, Value::List(xs)) => {
                !xs.iter().any(|x| matches!(x, Value::String(o) if o == s))
            }
            _ => holds(&Value::String((*s).to_owned()), op, operand),
        },
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => 0,
        Value::Int(_) | Value::Float(_) => 1,
        Value::Date(_) | Value::DateTime(_) => 2,
        Value::String(_) => 3,
        Value::List(_) => 4,
    }
}

/// Order with missing values last ([OP-FIND-7]).
fn cmp_opt(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => values::compare(x, y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// [OP-FIND-4] one scalar against the operand.
fn holds(v: &Value, op: Op, operand: &Value) -> bool {
    if let Value::List(items) = v {
        return match op {
            Op::Ne => !items.iter().any(|x| holds(x, Op::Eq, operand)),
            Op::NotIn => !items.iter().any(|x| holds(x, Op::In, operand)),
            _ => items.iter().any(|x| holds(x, op, operand)),
        };
    }
    match op {
        Op::Exists | Op::Missing => unreachable!("handled on the field"),
        Op::Ne => !holds(v, Op::Eq, operand),
        Op::NotIn => !holds(v, Op::In, operand),
        Op::In => match operand {
            Value::List(xs) => xs.iter().any(|x| holds(v, Op::Eq, x)),
            x => holds(v, Op::Eq, x),
        },
        Op::Between => match operand {
            Value::List(xs) if xs.len() == 2 => {
                holds(v, Op::Gte, &xs[0]) && holds(v, Op::Lte, &xs[1])
            }
            _ => false,
        },
        Op::Contains | Op::StartsWith => match (v, operand) {
            (Value::String(a), Value::String(b)) => {
                let (a, b) = (name_key(a), name_key(b));
                if op == Op::Contains {
                    a.contains(&b)
                } else {
                    a.starts_with(&b)
                }
            }
            _ => false,
        },
        _ if rank(v) != rank(operand) => false,
        _ if rank(v) == 2 => {
            let (Some((s, e)), Some((bs, be))) = (temporal_bounds(v), temporal_bounds(operand))
            else {
                return false;
            };
            match op {
                Op::Eq => bs <= s && e <= be,
                Op::Lt => e < bs,
                Op::Lte => e <= be,
                Op::Gt => s > be,
                Op::Gte => s >= bs,
                _ => false,
            }
        }
        Op::Eq => values::values_equal(v, operand),
        Op::Lt => values::compare(v, operand) == Ordering::Less,
        Op::Lte => values::compare(v, operand) != Ordering::Greater,
        Op::Gt => values::compare(v, operand) == Ordering::Greater,
        Op::Gte => values::compare(v, operand) != Ordering::Less,
    }
}

fn date_part(v: &Value, part: Part) -> Option<Value> {
    let (year, month) = match v {
        Value::Date(d) => (i64::from(d.year), d.month),
        Value::DateTime(t) => {
            let (y, m, _) = civil_from_days(t.ms.div_euclid(86_400_000));
            (y, m)
        }
        _ => return None,
    };
    Some(Value::Int(match part {
        Part::Year => year,
        Part::Month if month == 0 => return None,
        Part::Month => i64::from(month),
        Part::Decade => year.div_euclid(10) * 10,
        Part::Century => year.div_euclid(100) * 100,
    }))
}

impl<S: Store> Graph<S> {
    fn parse_expr(&self, target: Target, s: &str) -> Result<Expr, OpError> {
        let s = s.trim();
        let err = || {
            bad(
                "field",
                &format!("a field expression for this target, not {s:?}"),
            )
        };
        if let Some(inner) = s.strip_prefix("bucket(").and_then(|r| r.strip_suffix(')')) {
            let (e, w) = inner.rsplit_once(',').ok_or_else(err)?;
            let w: f64 = w
                .trim()
                .parse()
                .ok()
                .filter(|w: &f64| *w > 0.0)
                .ok_or_else(err)?;
            return Ok(Expr::Bucket(Box::new(self.parse_expr(target, e)?), w));
        }
        let (body, part) = match s.rsplit_once('.') {
            Some((b, "year")) => (b, Some(Part::Year)),
            Some((b, "month")) => (b, Some(Part::Month)),
            Some((b, "decade")) => (b, Some(Part::Decade)),
            Some((b, "century")) => (b, Some(Part::Century)),
            _ => (s, None),
        };
        use Target::*;
        let base = match (target, body) {
            (_, "id") => Base::Id,
            (_, "status") => Base::Status,
            (_, "created_at") => Base::CreatedAt,
            (_, "updated_at") => Base::UpdatedAt,
            (Nodes | Links, "evidence_count") => Base::EvidenceCount,
            (Nodes | Links, p) if p.starts_with("prop:") => Base::Prop(
                self.vocab_name(VocabKind::PropKey, &p[5..])
                    .map_err(|e| e.at(s))?,
            ),
            (Nodes | Links, "type") => Base::Type,
            (Nodes, "name") => Base::Name,
            (Nodes, "labels") => Base::Labels,
            (Nodes, "degree") => Base::Degree(None, Dir::Both),
            (Nodes, d) if d.starts_with("degree(") && d.ends_with(')') => {
                let args: Vec<&str> = d[7..d.len() - 1].split(',').collect();
                let t = self
                    .vocab_name(VocabKind::LinkType, args[0].trim())
                    .map_err(|e| e.at(s))?;
                let dir = match args.get(1) {
                    None => Dir::Both,
                    Some(a) => parse_dir(a).ok_or_else(err)?,
                };
                if args.len() > 2 {
                    return Err(err());
                }
                Base::Degree(Some(t), dir)
            }
            (Links, "from") => Base::From,
            (Links, "to") => Base::To,
            (Links, "from.type") => Base::FromType,
            (Links, "to.type") => Base::ToType,
            (Links, "directed") => Base::Directed,
            (Evidence, "kind") => Base::Kind,
            (Evidence, "title") => Base::Title,
            (Evidence, "uri") => Base::Uri,
            (Evidence, "author") => Base::Author,
            (Evidence, "publisher") => Base::Publisher,
            (Evidence, "language") => Base::Language,
            (Evidence, "source_date") => Base::SourceDate,
            (Evidence, "captured_at") => Base::CapturedAt,
            (Evidence, "citations") => Base::Citations,
            (Citations, "evidence") => Base::Evidence,
            (Citations, "target") => Base::CitTarget,
            (Citations, "target_kind") => Base::TargetKind,
            (Citations, "prop") => Base::CitProp,
            (Citations, "role") => Base::Role,
            (Citations, "confidence") => Base::Confidence,
            (Citations, "evidence.kind") => Base::EvidenceKind,
            _ => return Err(err()),
        };
        Ok(Expr::Field(base, part))
    }

    fn eval(&self, target: Target, idx: i64, e: &Expr) -> Option<Value> {
        match e {
            Expr::Bucket(inner, w) => {
                let x = num(&self.eval(target, idx, inner)?)?;
                let b = (x / w).floor() * w;
                Some(if b.fract() == 0.0 && b.abs() < 9.0e15 {
                    Value::Int(b as i64)
                } else {
                    Value::Float(b)
                })
            }
            Expr::Field(base, part) => {
                let v = self.eval_base(target, idx, base)?;
                match part {
                    None => Some(v),
                    Some(p) => date_part(&v, *p),
                }
            }
        }
    }

    /// Like `eval`, but borrows type names, names, stored properties and other stored text.
    fn eval_v<'a>(&'a self, target: Target, idx: i64, e: &Expr) -> Option<V<'a>> {
        let st = &self.state;
        if let Expr::Field(base, None) = e {
            match (target, base) {
                (Target::Nodes, Base::Type) => return st.nodes.get(&idx).map(|n| V::Str(&n.type_)),
                (Target::Nodes, Base::Name) => return st.nodes.get(&idx).map(|n| V::Str(&n.name)),
                (Target::Nodes, Base::Prop(k)) => {
                    return st.nodes.get(&idx).and_then(|n| n.props.get(k)).map(V::Ref);
                }
                (Target::Links, Base::Type) => return st.links.get(&idx).map(|l| V::Str(&l.type_)),
                (Target::Links, Base::Prop(k)) => {
                    return st.links.get(&idx).and_then(|l| l.props.get(k)).map(V::Ref);
                }
                (Target::Links, Base::FromType | Base::ToType) => {
                    let l = st.links.get(&idx)?;
                    let n = if *base == Base::FromType {
                        l.src
                    } else {
                        l.dst
                    };
                    return st.nodes.get(&n).map(|n| V::Str(&n.type_));
                }
                (Target::Evidence, Base::Kind) => {
                    return st.evidence.get(&idx).map(|e| V::Str(&e.kind));
                }
                (Target::Evidence, Base::SourceDate) => {
                    return st
                        .evidence
                        .get(&idx)
                        .and_then(|e| e.source_date.as_ref())
                        .map(V::Ref);
                }
                (Target::Citations, Base::Role) => {
                    return st.citations.get(&idx).map(|c| V::Str(c.role.name()));
                }
                (_, Base::Status) => return self.status_of(target, idx).map(|s| V::Str(s.name())),
                _ => {}
            }
        }
        self.eval(target, idx, e).map(V::Own)
    }

    fn status_of(&self, target: Target, idx: i64) -> Option<Status> {
        let st = &self.state;
        match target {
            Target::Nodes => st.nodes.get(&idx).map(|x| x.status),
            Target::Links => st.links.get(&idx).map(|x| x.status),
            Target::Evidence => st.evidence.get(&idx).map(|x| x.status),
            Target::Citations => st.citations.get(&idx).map(|x| x.status),
        }
    }

    fn meta_of(&self, target: Target, idx: i64) -> Option<crate::model::RecordMeta> {
        let st = &self.state;
        match target {
            Target::Nodes => st.nodes.get(&idx).map(|x| x.meta),
            Target::Links => st.links.get(&idx).map(|x| x.meta),
            Target::Evidence => st.evidence.get(&idx).map(|x| x.meta),
            Target::Citations => st.citations.get(&idx).map(|x| x.meta),
        }
    }

    fn eval_base(&self, target: Target, idx: i64, base: &Base) -> Option<Value> {
        let st = &self.state;
        let s = |x: &str| Some(Value::String(x.to_owned()));
        let opt = |x: &Option<String>| x.as_ref().map(|v| Value::String(v.clone()));
        let dt = |ms: i64| Some(Value::DateTime(DateTime { ms }));
        let kind = match target {
            Target::Nodes => ObjectKind::Node,
            Target::Links => ObjectKind::Link,
            Target::Evidence => ObjectKind::Evidence,
            Target::Citations => ObjectKind::Citation,
        };
        match base {
            Base::Id => return s(&id_string(kind, idx)),
            Base::Status => return s(self.status_of(target, idx)?.name()),
            Base::CreatedAt => return dt(self.meta_of(target, idx)?.created_at),
            Base::UpdatedAt => return dt(self.meta_of(target, idx)?.updated_at),
            Base::EvidenceCount => {
                let t = if target == Target::Nodes {
                    TargetKind::Node
                } else {
                    TargetKind::Link
                };
                return Some(Value::Int(self.sources_of(t, idx) as i64));
            }
            _ => {}
        }
        match target {
            Target::Nodes => {
                let n = st.nodes.get(&idx)?;
                match base {
                    Base::Prop(k) => n.props.get(k).cloned(),
                    Base::Type => s(&n.type_),
                    Base::Name => s(&n.name),
                    Base::Labels => (!n.labels.is_empty()).then(|| {
                        Value::List(n.labels.iter().map(|l| Value::String(l.clone())).collect())
                    }),
                    Base::Degree(t, dir) => {
                        let count = st
                            .adjacency
                            .get(&idx)
                            .into_iter()
                            .flatten()
                            .map(|l| &st.links[l])
                            .filter(|l| l.status == Status::Active)
                            .filter(|l| t.as_ref().is_none_or(|t| **t == *l.type_))
                            .filter(|l| {
                                !l.directed
                                    || match dir {
                                        Dir::Both => true,
                                        Dir::Out => l.src == idx,
                                        Dir::In => l.dst == idx,
                                    }
                            })
                            .count();
                        Some(Value::Int(count as i64))
                    }
                    _ => None,
                }
            }
            Target::Links => {
                let l = st.links.get(&idx)?;
                match base {
                    Base::Prop(k) => l.props.get(k).cloned(),
                    Base::Type => s(&l.type_),
                    Base::From => s(&id_string(ObjectKind::Node, l.src)),
                    Base::To => s(&id_string(ObjectKind::Node, l.dst)),
                    Base::FromType => st.nodes.get(&l.src).and_then(|n| s(&n.type_)),
                    Base::ToType => st.nodes.get(&l.dst).and_then(|n| s(&n.type_)),
                    Base::Directed => Some(Value::Bool(l.directed)),
                    _ => None,
                }
            }
            Target::Evidence => {
                let e = st.evidence.get(&idx)?;
                match base {
                    Base::Kind => s(&e.kind),
                    Base::Title => opt(&e.title),
                    Base::Uri => opt(&e.uri),
                    Base::Author => opt(&e.author),
                    Base::Publisher => opt(&e.publisher),
                    Base::Language => opt(&e.language),
                    Base::SourceDate => e.source_date.clone(),
                    Base::CapturedAt => dt(e.captured_at),
                    Base::Citations => {
                        let n = st
                            .cites_by_evidence
                            .get(&idx)
                            .into_iter()
                            .flatten()
                            .filter(|c| st.citations[c].status == Status::Active)
                            .count();
                        Some(Value::Int(n as i64))
                    }
                    _ => None,
                }
            }
            Target::Citations => {
                let c = st.citations.get(&idx)?;
                match base {
                    Base::Evidence => s(&id_string(ObjectKind::Evidence, c.evidence)),
                    Base::CitTarget => s(&id_string(c.target_kind.object_kind(), c.target)),
                    Base::TargetKind => s(match c.target_kind {
                        TargetKind::Node => "node",
                        TargetKind::Link => "link",
                    }),
                    Base::CitProp => opt(&c.prop),
                    Base::Role => s(c.role.name()),
                    Base::Confidence => c.confidence.map(Value::Float),
                    Base::EvidenceKind => st.evidence.get(&c.evidence).and_then(|e| s(&e.kind)),
                    _ => None,
                }
            }
        }
    }

    fn parse_cond(&self, target: Target, j: &Json) -> Result<Cond, OpError> {
        let o = j
            .as_object()
            .ok_or_else(|| bad("where", "a condition object"))?;
        let list = |v: &Json| -> Result<Vec<Cond>, OpError> {
            v.as_array()
                .ok_or_else(|| bad("where", "a list of conditions"))?
                .iter()
                .map(|c| self.parse_cond(target, c))
                .collect()
        };
        if let Some(v) = o.get("all") {
            return Ok(Cond::All(list(v)?));
        }
        if let Some(v) = o.get("any") {
            return Ok(Cond::Any(list(v)?));
        }
        if let Some(v) = o.get("not") {
            return Ok(Cond::Not(Box::new(self.parse_cond(target, v)?)));
        }
        if let Some(v) = o.get("linked") {
            if target != Target::Nodes {
                return Err(bad("linked", "only with target nodes"));
            }
            let l = v.as_object().ok_or_else(|| bad("linked", "an object"))?;
            let t = match l.get("link_type").and_then(Json::as_str) {
                None => None,
                Some(t) => Some(
                    self.vocab_name(VocabKind::LinkType, t)
                        .map_err(|e| e.at("linked.link_type"))?,
                ),
            };
            let dir = match l.get("direction").and_then(Json::as_str) {
                None => Dir::Both,
                Some(d) => {
                    parse_dir(d).ok_or_else(|| bad("linked.direction", "out, in or both"))?
                }
            };
            let node = match self.resolve_any(
                l.get("node")
                    .ok_or_else(|| bad("linked.node", "a node reference"))?,
            )? {
                (ObjectKind::Node, n, _) => n,
                _ => return Err(bad("linked.node", "a node reference")),
            };
            return Ok(Cond::Linked(t, dir, node));
        }
        let field = o
            .get("field")
            .and_then(Json::as_str)
            .ok_or_else(|| bad("where", "{field, op, value}, all, any, not or linked"))?;
        let expr = self.parse_expr(target, field)?;
        let op = match o.get("op").and_then(Json::as_str) {
            Some("exists") => Op::Exists,
            Some("missing") => Op::Missing,
            Some("eq") => Op::Eq,
            Some("ne") => Op::Ne,
            Some("lt") => Op::Lt,
            Some("lte") => Op::Lte,
            Some("gt") => Op::Gt,
            Some("gte") => Op::Gte,
            Some("between") => Op::Between,
            Some("in") => Op::In,
            Some("not_in") => Op::NotIn,
            Some("contains") => Op::Contains,
            Some("starts_with") => Op::StartsWith,
            _ => return Err(bad("op", "a filter operator")),
        };
        let value = if matches!(op, Op::Exists | Op::Missing) {
            None
        } else {
            let v = o.get("value").ok_or_else(|| bad("value", "a value"))?;
            match values::parse(v).map_err(|r| bad("value", r.code()))? {
                Parsed::Value(v) => Some(v),
                Parsed::Remove => return Err(bad("value", "a value")),
            }
        };
        if matches!(op, Op::Between) && !matches!(&value, Some(Value::List(x)) if x.len() == 2) {
            return Err(bad("value", "[low, high] for between"));
        }
        // A type name given to compare with must follow the graph's naming policy, as in a write;
        // otherwise the condition could never match, and the error names the expected spelling.
        if let (Expr::Field(Base::Type, None), Some(v), Op::Eq | Op::Ne | Op::In | Op::NotIn) =
            (&expr, &value, op)
        {
            let kind = if target == Target::Links {
                VocabKind::LinkType
            } else {
                VocabKind::NodeType
            };
            let names: Vec<&String> = match v {
                Value::String(s) => vec![s],
                Value::List(xs) => xs
                    .iter()
                    .filter_map(|x| match x {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            };
            for n in names {
                self.vocab_name(kind, n).map_err(|e| e.at("value"))?;
            }
        }
        Ok(Cond::Cmp(expr, op, value))
    }

    /// Candidates from the type index when the condition fixes the type of active nodes or
    /// links: (ids in internal-number order, whether the type alone decides the condition).
    fn type_candidates(&self, target: Target, cond: Option<&Cond>) -> Option<(Vec<i64>, bool)> {
        let index = match target {
            Target::Nodes => &self.state.nodes_by_type,
            Target::Links => &self.state.links_by_type,
            _ => return None,
        };
        let types = |c: &Cond| -> Option<Vec<String>> {
            match c {
                Cond::Cmp(Expr::Field(Base::Type, None), Op::Eq, Some(Value::String(t))) => {
                    Some(vec![t.clone()])
                }
                Cond::Cmp(Expr::Field(Base::Type, None), Op::In, Some(Value::List(xs))) => xs
                    .iter()
                    .map(|x| match x {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => None,
            }
        };
        let (ts, exact) = match cond? {
            Cond::All(cs) => (cs.iter().find_map(types)?, cs.len() == 1),
            c => (types(c)?, true),
        };
        let mut ids: Vec<i64> = ts
            .iter()
            .filter_map(|t| index.get(t.as_str()))
            .flat_map(|s| s.iter().copied())
            .collect();
        if ts.len() > 1 {
            ids.sort_unstable();
            ids.dedup();
        }
        Some((ids, exact))
    }

    fn test(&self, target: Target, idx: i64, c: &Cond) -> bool {
        match c {
            Cond::All(cs) => cs.iter().all(|c| self.test(target, idx, c)),
            Cond::Any(cs) => cs.iter().any(|c| self.test(target, idx, c)),
            Cond::Not(c) => !self.test(target, idx, c),
            Cond::Linked(t, dir, node) => {
                let st = &self.state;
                st.adjacency.get(&idx).into_iter().flatten().any(|l| {
                    let l = &st.links[l];
                    let other = if l.src == idx { l.dst } else { l.src };
                    l.status == Status::Active
                        && other == *node
                        && t.as_ref().is_none_or(|t| **t == *l.type_)
                        && (!l.directed
                            || match dir {
                                Dir::Both => true,
                                Dir::Out => l.src == idx,
                                Dir::In => l.dst == idx,
                            })
                })
            }
            Cond::Cmp(e, op, operand) => {
                let v = self.eval_v(target, idx, e);
                match (op, v, operand) {
                    (Op::Exists, v, _) => v.is_some(),
                    (Op::Missing, v, _) => v.is_none(),
                    (Op::Ne | Op::NotIn, None, _) => true,
                    (_, None, _) => false,
                    (op, Some(v), Some(x)) => holds_v(&v, *op, x),
                    (_, Some(_), None) => false,
                }
            }
        }
    }

    /// Operation `find` ([OP-FIND-1] .. [OP-FIND-8]).
    pub fn find(&self, request: &Json) -> Result<Json, OpError> {
        let req = request
            .as_object()
            .ok_or_else(|| bad("request", "an object"))?;
        let target = match req.get("target").and_then(Json::as_str) {
            Some("nodes") => Target::Nodes,
            Some("links") => Target::Links,
            Some("evidence") => Target::Evidence,
            Some("citations") => Target::Citations,
            _ => return Err(bad("target", "nodes, links, evidence or citations")),
        };
        let cond = req
            .get("where")
            .map(|w| self.parse_cond(target, w))
            .transpose()?;
        let retracted = req
            .get("include_retracted")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let exprs = |key: &str, max: usize| -> Result<Vec<(String, Expr)>, OpError> {
            match req.get(key) {
                None => Ok(Vec::new()),
                Some(Json::Array(a)) if a.len() <= max => a
                    .iter()
                    .map(|v| {
                        let s = v.as_str().ok_or_else(|| bad(key, "field expressions"))?;
                        Ok((s.to_owned(), self.parse_expr(target, s)?))
                    })
                    .collect(),
                Some(_) => Err(bad(
                    key,
                    &format!("a list of at most {max} field expressions"),
                )),
            }
        };
        let fields = exprs("fields", 10)?;
        let group_by = exprs("group_by", 3)?;
        let mut aggs = Vec::new();
        match req.get("aggregate") {
            None => {}
            Some(Json::Array(a)) if a.len() <= 10 => {
                for x in a {
                    let f = x.get("fn").and_then(Json::as_str).unwrap_or("");
                    let of = x.get("of").and_then(Json::as_str);
                    let (fun, name) = match (f, of) {
                        ("count", None) => (AggFn::Count, "count".to_owned()),
                        ("count", Some(o)) => (AggFn::CountOf, format!("count({o})")),
                        ("count_distinct", Some(o)) => {
                            (AggFn::CountDistinct, format!("count_distinct({o})"))
                        }
                        ("sum", Some(o)) => (AggFn::Sum, format!("sum({o})")),
                        ("avg", Some(o)) => (AggFn::Avg, format!("avg({o})")),
                        ("min", Some(o)) => (AggFn::Min, format!("min({o})")),
                        ("max", Some(o)) => (AggFn::Max, format!("max({o})")),
                        ("median", Some(o)) => (AggFn::Median, format!("median({o})")),
                        _ => return Err(bad("aggregate", "{fn, of} with a known function")),
                    };
                    let of = of.map(|o| self.parse_expr(target, o)).transpose()?;
                    aggs.push(Agg { f: fun, of, name });
                }
            }
            Some(_) => return Err(bad("aggregate", "a list of at most 10 aggregates")),
        }
        let limit = match req.get("limit") {
            None => 20,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=1000).contains(n))
                .ok_or_else(|| bad("limit", "1 to 1000"))? as usize,
        };
        let offset = match req.get("cursor") {
            None | Some(Json::Null) => 0,
            Some(v) => v
                .as_str()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| bad("cursor", "a next_cursor value"))?,
        };
        let sort: Vec<(String, bool)> = match req.get("sort") {
            None => Vec::new(),
            Some(Json::Array(a)) if a.len() <= 3 => a
                .iter()
                .map(|s| {
                    let by = s
                        .get("by")
                        .and_then(Json::as_str)
                        .ok_or_else(|| bad("sort", "{by, order}"))?;
                    let desc = match s.get("order").and_then(Json::as_str) {
                        None | Some("asc") => false,
                        Some("desc") => true,
                        _ => return Err(bad("sort.order", "asc or desc")),
                    };
                    Ok((by.to_owned(), desc))
                })
                .collect::<Result<_, OpError>>()?,
            Some(_) => return Err(bad("sort", "a list of at most 3 sort keys")),
        };

        // Counts of active objects per type are the sizes of the type index.
        let count_only = aggs.iter().all(|a| a.f == AggFn::Count);
        if !retracted
            && cond.is_none()
            && count_only
            && matches!(target, Target::Nodes | Target::Links)
            && matches!(group_by.as_slice(), [(_, Expr::Field(Base::Type, None))])
        {
            let index = if target == Target::Nodes {
                &self.state.nodes_by_type
            } else {
                &self.state.links_by_type
            };
            let aggs = if aggs.is_empty() {
                vec![Agg {
                    f: AggFn::Count,
                    of: None,
                    name: "count".into(),
                }]
            } else {
                aggs
            };
            let out = index
                .iter()
                .filter(|(_, s)| !s.is_empty())
                .map(|(t, s)| {
                    let n = s.len() as u64;
                    let m: Map<String, Json> =
                        aggs.iter().map(|a| (a.name.clone(), json!(n))).collect();
                    (vec![Some(Value::String(t.to_string()))], n, m)
                })
                .collect();
            return self.finish_groups(out, &group_by, &aggs, &sort, offset, limit);
        }

        // Matching objects, by internal number. A condition that fixes the type starts from the
        // type index instead of scanning the table.
        let st = &self.state;
        let ok_status = |s: Status| s == Status::Active || retracted && s == Status::Retracted;
        let planned = if retracted {
            None
        } else {
            self.type_candidates(target, cond.as_ref())
        };
        let matching: Vec<i64> = match planned {
            Some((ids, true)) => ids,
            Some((ids, false)) => {
                let c = cond.as_ref().expect("planned from a condition");
                ids.into_iter()
                    .filter(|&i| self.test(target, i, c))
                    .collect()
            }
            None => {
                let keep = |i: i64, s: Status| {
                    ok_status(s) && cond.as_ref().is_none_or(|c| self.test(target, i, c))
                };
                match target {
                    Target::Nodes => st
                        .nodes
                        .iter()
                        .filter(|(i, n)| keep(*i, n.status))
                        .map(|(i, _)| i)
                        .collect(),
                    Target::Links => st
                        .links
                        .iter()
                        .filter(|(i, l)| keep(*i, l.status))
                        .map(|(i, _)| i)
                        .collect(),
                    Target::Evidence => st
                        .evidence
                        .iter()
                        .filter(|(i, e)| keep(*i, e.status))
                        .map(|(i, _)| i)
                        .collect(),
                    Target::Citations => st
                        .citations
                        .iter()
                        .filter(|(i, c)| keep(*i, c.status))
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
        };

        if group_by.is_empty() && aggs.is_empty() {
            return self.rows(target, matching, &fields, &sort, offset, limit);
        }
        if aggs.is_empty() {
            aggs.push(Agg {
                f: AggFn::Count,
                of: None,
                name: "count".into(),
            });
        }
        // Counting without groups needs no per-object work.
        if group_by.is_empty() && aggs.iter().all(|a| a.f == AggFn::Count) {
            let n = matching.len() as u64;
            let m: Map<String, Json> = aggs.iter().map(|a| (a.name.clone(), json!(n))).collect();
            return self.finish_groups(
                vec![(Vec::new(), n, m)],
                &group_by,
                &aggs,
                &sort,
                offset,
                limit,
            );
        }
        self.groups(target, &matching, &group_by, &aggs, &sort, offset, limit)
    }

    fn rows(
        &self,
        target: Target,
        mut matching: Vec<i64>,
        fields: &[(String, Expr)],
        sort: &[(String, bool)],
        offset: usize,
        limit: usize,
    ) -> Result<Json, OpError> {
        let keys: Vec<(Expr, bool)> = sort
            .iter()
            .map(|(by, desc)| Ok((self.parse_expr(target, by)?, *desc)))
            .collect::<Result<_, OpError>>()?;
        if !keys.is_empty() {
            let mut keyed: Vec<(Vec<Option<Value>>, i64)> = matching
                .iter()
                .map(|&i| {
                    (
                        keys.iter().map(|(e, _)| self.eval(target, i, e)).collect(),
                        i,
                    )
                })
                .collect();
            keyed.sort_by(|a, b| {
                for (k, (_, desc)) in keys.iter().enumerate() {
                    let (x, y) = (a.0[k].as_ref(), b.0[k].as_ref());
                    let o = match (x, y, desc) {
                        (Some(_), Some(_), true) => cmp_opt(y, x),
                        _ => cmp_opt(x, y),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.1.cmp(&b.1)
            });
            matching = keyed.into_iter().map(|k| k.1).collect();
        }
        let total = matching.len();
        let kind = match target {
            Target::Nodes => ObjectKind::Node,
            Target::Links => ObjectKind::Link,
            Target::Evidence => ObjectKind::Evidence,
            Target::Citations => ObjectKind::Citation,
        };
        let keep = BRIEF
            .iter()
            .find(|(k, _)| *k == kind.name())
            .map_or(&[][..], |(_, f)| f);
        let rows: Vec<Json> = matching
            .iter()
            .skip(offset)
            .take(limit)
            .map(|&i| {
                let row: Row = self.state.get(kind, i).expect("matching object");
                let mut m = record_json(&row, &|a| self.state.actor_name(a));
                m.retain(|k, _| keep.contains(&k.as_str()));
                if !fields.is_empty() {
                    let values: Map<String, Json> = fields
                        .iter()
                        .map(|(name, e)| {
                            (
                                name.clone(),
                                self.eval(target, i, e).map_or(Json::Null, |v| v.to_json()),
                            )
                        })
                        .collect();
                    m.insert("values".into(), Json::Object(values));
                }
                Json::Object(m)
            })
            .collect();
        let next = (offset + limit < total).then(|| (offset + limit).to_string());
        Ok(json!({ "rows": rows, "total": total, "next_cursor": next }))
    }

    #[allow(clippy::too_many_arguments)]
    fn groups(
        &self,
        target: Target,
        matching: &[i64],
        group_by: &[(String, Expr)],
        aggs: &[Agg],
        sort: &[(String, bool)],
        offset: usize,
        limit: usize,
    ) -> Result<Json, OpError> {
        struct Acc<'a> {
            /// The first object of the group: its values are shown as the group key.
            first: i64,
            count: u64,
            per: Vec<AggState<'a>>,
        }
        #[derive(Default)]
        struct AggState<'a> {
            n: u64,
            distinct: HashSet<Key<'a>>,
            sum: f64,
            isum: i64,
            all_int: bool,
            best: Option<Value>,
            nums: Vec<f64>,
        }
        let mut groups: HashMap<GroupKey<'_>, Acc<'_>> = HashMap::new();
        let key_part = |i: i64, e: &Expr| self.eval_v(target, i, e).map_or(Key::Null, V::into_key);
        for &i in matching {
            let key = match group_by {
                [(_, e)] => GroupKey::One(key_part(i, e)),
                many => GroupKey::Many(many.iter().map(|(_, e)| key_part(i, e)).collect()),
            };
            let acc = groups.entry(key).or_insert_with(|| Acc {
                first: i,
                count: 0,
                per: aggs
                    .iter()
                    .map(|_| AggState {
                        all_int: true,
                        ..Default::default()
                    })
                    .collect(),
            });
            acc.count += 1;
            for (a, s) in aggs.iter().zip(acc.per.iter_mut()) {
                let Some(of) = &a.of else { continue };
                let Some(v) = self.eval_v(target, i, of) else {
                    continue;
                };
                s.n += 1;
                match a.f {
                    AggFn::CountDistinct => {
                        s.distinct.insert(v.into_key());
                    }
                    AggFn::Sum | AggFn::Avg | AggFn::Median => {
                        if let Some(x) = v.num() {
                            s.sum += x;
                            match v.is_int() {
                                Some(k) => s.isum = s.isum.saturating_add(k),
                                None => s.all_int = false,
                            }
                            if a.f == AggFn::Median {
                                s.nums.push(x);
                            }
                        } else {
                            s.n -= 1;
                        }
                    }
                    AggFn::Min | AggFn::Max => {
                        let val = v.value();
                        let better = s.best.as_ref().is_none_or(|b| {
                            let o = values::compare(&val, b);
                            if a.f == AggFn::Min {
                                o == Ordering::Less
                            } else {
                                o == Ordering::Greater
                            }
                        });
                        if better {
                            s.best = Some(val.into_owned());
                        }
                    }
                    AggFn::Count | AggFn::CountOf => {}
                }
            }
        }
        // Without `group_by` there is always one group, also over no objects ([OP-FIND-5]).
        if group_by.is_empty() && groups.is_empty() {
            let m: Map<String, Json> = aggs
                .iter()
                .map(|a| {
                    let v = match a.f {
                        AggFn::Count | AggFn::CountOf | AggFn::CountDistinct => json!(0),
                        _ => Json::Null,
                    };
                    (a.name.clone(), v)
                })
                .collect();
            return self.finish_groups(
                vec![(Vec::new(), 0, m)],
                group_by,
                aggs,
                sort,
                offset,
                limit,
            );
        }
        // Group rows: key values and aggregate results.
        let out: Vec<GroupRow> = groups
            .into_values()
            .map(|acc| {
                let mut m = Map::new();
                for (a, s) in aggs.iter().zip(acc.per) {
                    let v = match a.f {
                        AggFn::Count => json!(acc.count),
                        AggFn::CountOf => json!(s.n),
                        AggFn::CountDistinct => json!(s.distinct.len()),
                        AggFn::Sum if s.n == 0 => Json::Null,
                        AggFn::Sum if s.all_int => json!(s.isum),
                        AggFn::Sum => json!(s.sum),
                        AggFn::Avg if s.n == 0 => Json::Null,
                        AggFn::Avg => json!(s.sum / s.n as f64),
                        AggFn::Min | AggFn::Max => s.best.map_or(Json::Null, |v| v.to_json()),
                        AggFn::Median => {
                            let mut xs = s.nums;
                            if xs.is_empty() {
                                Json::Null
                            } else {
                                xs.sort_by(f64::total_cmp);
                                let n = xs.len();
                                json!(if n % 2 == 1 {
                                    xs[n / 2]
                                } else {
                                    (xs[n / 2 - 1] + xs[n / 2]) / 2.0
                                })
                            }
                        }
                    };
                    m.insert(a.name.clone(), v);
                }
                let values = group_by
                    .iter()
                    .map(|(_, e)| self.eval(target, acc.first, e))
                    .collect();
                (values, acc.count, m)
            })
            .collect();
        self.finish_groups(out, group_by, aggs, sort, offset, limit)
    }

    /// [OP-FIND-7] [OP-FIND-8] for groups given as (key values, count, aggregate results).
    fn finish_groups(
        &self,
        mut out: Vec<GroupRow>,
        group_by: &[(String, Expr)],
        aggs: &[Agg],
        sort: &[(String, bool)],
        offset: usize,
        limit: usize,
    ) -> Result<Json, OpError> {
        let sort_keys: Vec<(Result<usize, String>, bool)> = sort
            .iter()
            .map(|(by, desc)| {
                if let Some(k) = group_by.iter().position(|(name, _)| name == by) {
                    Ok((Ok(k), *desc))
                } else if aggs.iter().any(|a| a.name == *by) {
                    Ok((Err(by.clone()), *desc))
                } else {
                    Err(bad("sort.by", "a group_by expression or an aggregate name"))
                }
            })
            .collect::<Result<_, OpError>>()?;
        let agg_value = |m: &Map<String, Json>, name: &str| -> Option<Value> {
            match m.get(name) {
                None | Some(Json::Null) => None,
                Some(j) => match values::parse(j) {
                    Ok(Parsed::Value(v)) => Some(v),
                    _ => None,
                },
            }
        };
        out.sort_by(|a, b| {
            for (key, desc) in &sort_keys {
                let (x, y) = match key {
                    Ok(k) => (a.0[*k].clone(), b.0[*k].clone()),
                    Err(name) => (agg_value(&a.2, name), agg_value(&b.2, name)),
                };
                let o = match (&x, &y, desc) {
                    (Some(_), Some(_), true) => cmp_opt(y.as_ref(), x.as_ref()),
                    _ => cmp_opt(x.as_ref(), y.as_ref()),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            b.1.cmp(&a.1).then_with(|| {
                for (x, y) in a.0.iter().zip(&b.0) {
                    let o = cmp_opt(x.as_ref(), y.as_ref());
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            })
        });
        let total = out.len();
        let groups: Vec<Json> = out
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(vals, _, aggs_json)| {
                let key: Map<String, Json> = group_by
                    .iter()
                    .zip(vals)
                    .map(|((name, _), v)| (name.clone(), v.map_or(Json::Null, |v| v.to_json())))
                    .collect();
                let mut m = Map::new();
                m.insert("key".into(), Json::Object(key));
                m.extend(aggs_json);
                Json::Object(m)
            })
            .collect();
        let next = (offset + limit < total).then(|| (offset + limit).to_string());
        Ok(json!({ "groups": groups, "total": total, "next_cursor": next }))
    }
}
