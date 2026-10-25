//! Operation `resolve` — spec/api/operations/resolve.md, spec/algorithms/resolve-scoring.md.

use crate::engine::{Graph, OpError};
use crate::ids::ObjectKind;
use crate::model::id_string;
use crate::name_key::{check_name, name_key};
use crate::store::Store;
use crate::text::nfc;
use crate::values::{self, Parsed, Value};
use crate::vocab::VocabKind;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap};

const MAX_SCORED: usize = 200;
const EPS: f64 = 1e-9;

fn bad(index: usize, field: &str, expected: &str) -> OpError {
    OpError::new(
        "resolve.invalid_field",
        format!("candidates[{index}].{field}: expected {expected}"),
    )
}

/// [RES-5]
pub fn hint_agrees(hint: &Value, stored: &Value) -> bool {
    match (hint, stored) {
        (Value::Date(a), Value::Date(b)) => {
            a.year == b.year
                && (a.month == 0 || b.month == 0 || a.month == b.month)
                && (a.day == 0 || b.day == 0 || a.day == b.day)
        }
        (Value::String(a), Value::String(b)) => name_key(a) == name_key(b),
        _ => values::values_equal(hint, stored),
    }
}

/// [RES-7] verdict from the scores of the scored nodes, highest first.
pub fn verdict(scores: &[f64]) -> &'static str {
    let plausible: Vec<f64> = scores.iter().copied().filter(|s| *s >= 0.6 - EPS).collect();
    match plausible.as_slice() {
        [] => "none",
        [first, rest @ ..] => {
            let clear = rest
                .first()
                .is_none_or(|second| *second < first - 0.1 - EPS);
            if *first >= 0.8 - EPS && clear {
                "match"
            } else {
                "ambiguous"
            }
        }
    }
}

/// Name part of a node's score ([RES-2]) and its `why` text.
struct NameScore {
    score: f64,
    why: String,
    /// Name or alias equal to the query: fuzzy similarity no longer applies ([RES-2]).
    exact: bool,
}

impl<S: Store> Graph<S> {
    /// Operation `resolve` ([OP-RESOLVE-1], [OP-RESOLVE-2]).
    pub fn resolve(&self, request: &Json) -> Result<Json, OpError> {
        let cands = request
            .get("candidates")
            .and_then(Json::as_array)
            .filter(|a| (1..=100).contains(&a.len()))
            .ok_or_else(|| {
                OpError::new(
                    "resolve.invalid_field",
                    "candidates: expected a list of 1 to 100 entities",
                )
            })?;
        let mut results = Vec::with_capacity(cands.len());
        for (i, c) in cands.iter().enumerate() {
            let o = c.as_object().ok_or_else(|| bad(i, "", "an object"))?;
            let name = o
                .get("name")
                .and_then(Json::as_str)
                .map(nfc)
                .ok_or_else(|| bad(i, "name", "a string"))?;
            check_name(&name).map_err(|r| bad(i, "name", r.code()))?;
            let type_ = match o.get("type") {
                None | Some(Json::Null) => None,
                Some(Json::String(t)) => Some(
                    self.canon(VocabKind::NodeType, t)
                        .map_err(|r| bad(i, "type", r.code()))?,
                ),
                Some(_) => return Err(bad(i, "type", "a node type")),
            };
            let mut hints = BTreeMap::new();
            match o.get("hints") {
                None | Some(Json::Null) => {}
                Some(Json::Object(h)) => {
                    for (k, v) in h {
                        let key = self
                            .canon(VocabKind::PropKey, k)
                            .map_err(|r| bad(i, &format!("hints.{k}"), r.code()))?;
                        match values::parse(v)
                            .map_err(|r| bad(i, &format!("hints.{k}"), r.code()))?
                        {
                            Parsed::Value(v) => {
                                hints.insert(key, v);
                            }
                            Parsed::Remove => return Err(bad(i, &format!("hints.{k}"), "a value")),
                        }
                    }
                }
                Some(_) => return Err(bad(i, "hints", "an object")),
            }
            results.push(self.resolve_one(&name, type_.as_deref(), &hints));
        }
        Ok(json!({ "results": results }))
    }

    fn resolve_one(
        &self,
        name: &str,
        type_: Option<&str>,
        hints: &BTreeMap<String, Value>,
    ) -> Json {
        let st = &self.state;
        let q = name_key(name);
        // [RES-1] [RES-2] candidates with their name scores.
        let mut names: HashMap<i64, NameScore> = HashMap::new();
        // Without aliases the matching key can only be the name.
        let is_name = |n: i64, key: &str| {
            let node = &st.nodes[&n];
            node.aliases.is_empty() || name_key(&node.name) == key
        };
        if let Some(set) = st.names.get(&q) {
            for &n in set {
                let (score, why) = if is_name(n, &q) {
                    (1.0, "name exact")
                } else {
                    (0.95, "alias exact")
                };
                names.insert(
                    n,
                    NameScore {
                        score,
                        why: why.into(),
                        exact: true,
                    },
                );
            }
        }
        let mut keys = st.names.fuzzy(&q, 0.4);
        keys.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        let mut stop_below = None;
        for (key, sim) in keys {
            if stop_below.is_some_and(|s| sim < s) {
                break;
            }
            let score = (sim + 0.2).min(0.9);
            for &n in st.names.get(key).into_iter().flatten() {
                let part = if is_name(n, key) { "name" } else { "alias" };
                let better = names
                    .get(&n)
                    .is_none_or(|old| !old.exact && score > old.score + EPS);
                if better {
                    names.insert(
                        n,
                        NameScore {
                            score,
                            why: format!("{part} fuzzy {sim:.2}"),
                            exact: false,
                        },
                    );
                }
            }
            if names.len() >= MAX_SCORED && stop_below.is_none() {
                stop_below = Some(sim);
            }
        }
        let mut ranked: Vec<(i64, NameScore)> = names.into_iter().collect();
        ranked.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)));
        ranked.truncate(MAX_SCORED);

        // [RES-3] .. [RES-6]
        let mut scored: Vec<(f64, i64, Vec<String>)> = ranked
            .into_iter()
            .map(|(n, ns)| {
                let node = &st.nodes[&n];
                let mut why = vec![ns.why];
                let mut factor = 1.0;
                if type_.is_some_and(|t| t != &*node.type_) {
                    factor = 0.7;
                    why.push(format!("type differs ({})", node.type_));
                }
                let (mut agree, mut conflict) = (0.0, 0.0);
                for (k, h) in hints {
                    if let Some(v) = node.props.get(k) {
                        if hint_agrees(h, v) {
                            agree += 1.0;
                            why.push(format!("{k} matches"));
                        } else {
                            conflict += 1.0;
                            why.push(format!("{k} differs"));
                        }
                    }
                }
                let score = (ns.score * factor + 0.1 * agree - 0.2 * conflict).clamp(0.0, 1.0);
                (score, n, why)
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let scores: Vec<f64> = scored.iter().map(|s| s.0).collect();
        let v = verdict(&scores);
        let node_json = |(score, n, why): &(f64, i64, Vec<String>)| {
            let node = &st.nodes[n];
            json!({
                "id": id_string(ObjectKind::Node, *n), "type": &*node.type_, "name": node.name,
                "score": (score * 1000.0).round() / 1000.0, "why": why,
            })
        };
        match v {
            "none" => json!({ "verdict": "none" }),
            "match" => json!({ "verdict": "match", "best": node_json(&scored[0]) }),
            _ => {
                let cands: Vec<Json> = scored
                    .iter()
                    .filter(|s| s.0 >= 0.6 - EPS)
                    .take(5)
                    .map(node_json)
                    .collect();
                json!({ "verdict": "ambiguous", "best": cands[0], "candidates": cands })
            }
        }
    }
}
