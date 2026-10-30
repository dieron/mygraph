//! Compares MyGraph's answers with Neo4j's on the same LDBC graph (docs/11-benchmarks.md, group
//! 1). Random `neighbors`, `paths` and `find` requests are each paired with a Cypher query meant to
//! return the same node set, count or groups; both results are normalized (node IDs to keys,
//! order removed, temporal values and numbers in one form) and compared.
//!
//! Neo4j stores `KNOWS` directed (once per pair, as listed) while MyGraph's `KNOWS` is undirected,
//! so the Cypher side ignores direction for `KNOWS`.

use crate::archive::Result;
use mygraph_core::engine::Graph;
use mygraph_core::ids::ObjectKind;
use mygraph_core::model::id_string;
use mygraph_store_sqlite::GraphFile;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashMap};

/// SplitMix64, as in `mygraph-bench`, so a seed fixes every case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
}

const TYPES: [&str; 8] = [
    "Person",
    "Forum",
    "Post",
    "Comment",
    "Organisation",
    "Place",
    "Tag",
    "TagClass",
];
const LINK_TYPES: [&str; 15] = [
    "CONTAINER_OF",
    "HAS_CREATOR",
    "HAS_INTEREST",
    "HAS_MEMBER",
    "HAS_MODERATOR",
    "HAS_TAG",
    "HAS_TYPE",
    "IS_LOCATED_IN",
    "IS_PART_OF",
    "IS_SUBCLASS_OF",
    "KNOWS",
    "LIKES",
    "REPLY_OF",
    "STUDY_AT",
    "WORK_AT",
];
/// Link types `paths` cases walk: selective sets, so shortest paths stay few.
const PATH_LINK_SETS: [&[&str]; 5] = [
    &["KNOWS"],
    &["KNOWS", "STUDY_AT", "WORK_AT"],
    &["HAS_CREATOR", "REPLY_OF"],
    &["HAS_MEMBER", "KNOWS"],
    &["HAS_CREATOR", "LIKES", "KNOWS"],
];

pub struct Neo4j {
    url: String,
    agent: ureq::Agent,
}

impl Neo4j {
    pub fn new(base: &str, db: &str) -> Self {
        Self {
            url: format!("{}/db/{db}/tx/commit", base.trim_end_matches('/')),
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    /// The rows of one statement; a Neo4j error (a timeout, say) comes back as `Err`.
    fn rows(&self, cypher: &str) -> Result<Vec<Vec<Json>>> {
        let body = json!({ "statements": [{ "statement": cypher }] });
        let res: Json = self.agent.post(&self.url).send_json(body)?.into_json()?;
        if let Some(e) = res["errors"].as_array().and_then(|e| e.first()) {
            return Err(format!("neo4j: {}", e["message"]).into());
        }
        Ok(res["results"][0]["data"]
            .as_array()
            .ok_or("no data")?
            .iter()
            .map(|d| d["row"].as_array().cloned().unwrap_or_default())
            .collect())
    }
}

/// A Cypher string literal.
fn lit(s: &str) -> String {
    serde_json::to_string(s).expect("string")
}

fn list_lit(xs: &[String]) -> String {
    format!(
        "[{}]",
        xs.iter().map(|x| lit(x)).collect::<Vec<_>>().join(", ")
    )
}

/// One form for both sides: `{"$date": s}` → s, date-times with exactly three fraction digits,
/// numbers as integers when whole and rounded to 9 decimals otherwise, lists of sets sorted.
fn norm(v: &Json) -> Json {
    match v {
        Json::Object(o) => {
            if let Some(s) = o
                .get("$datetime")
                .or_else(|| o.get("$date"))
                .and_then(Json::as_str)
            {
                return norm(&json!(s));
            }
            Json::Object(o.iter().map(|(k, v)| (k.clone(), norm(v))).collect())
        }
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Number(n) => {
            let f: f64 = n.to_string().parse().unwrap_or(f64::NAN);
            if f.fract() == 0.0 && f.abs() < 9e15 {
                json!(f as i64)
            } else {
                json!((f * 1e9).round() / 1e9)
            }
        }
        Json::String(s) => json!(canonical_datetime(s).unwrap_or_else(|| s.clone())),
        _ => v.clone(),
    }
}

/// `2011-08-17T14:26:59.96Z` (Neo4j trims zeros) → `2011-08-17T14:26:59.960Z`.
fn canonical_datetime(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if s.len() < 20 || b[10] != b'T' || !s.ends_with('Z') || b[4] != b'-' || b[13] != b':' {
        return None;
    }
    let (head, frac) = (&s[..19], &s[19..s.len() - 1]);
    let digits = frac.strip_prefix('.').unwrap_or("");
    if !frac.is_empty() && (digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let ms: String = digits.chars().chain("000".chars()).take(3).collect();
    Some(format!("{head}.{ms}Z"))
}

fn sorted(mut v: Vec<Json>) -> Json {
    v.sort_by_key(|x| x.to_string());
    Json::Array(v)
}

/// A start of a period and the start of the next one, for a `YYYY`, `YYYY-MM` or `YYYY-MM-DD`
/// operand: the interval it denotes ([VAL-DATE-5]) as half-open days.
fn period(s: &str) -> (String, String) {
    let p: Vec<i64> = s.split('-').map(|x| x.parse().unwrap_or(1)).collect();
    let days_in = |y: i64, m: i64| match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    };
    let f = |y: i64, m: i64, d: i64| format!("{y:04}-{m:02}-{d:02}");
    match p.as_slice() {
        [y] => (f(*y, 1, 1), f(y + 1, 1, 1)),
        [y, m] => (
            f(*y, *m, 1),
            if *m == 12 {
                f(y + 1, 1, 1)
            } else {
                f(*y, m + 1, 1)
            },
        ),
        [y, m, d] => {
            let next = if *d < days_in(*y, *m) {
                f(*y, *m, d + 1)
            } else if *m < 12 {
                f(*y, m + 1, 1)
            } else {
                f(y + 1, 1, 1)
            };
            (f(*y, *m, *d), next)
        }
        _ => (s.to_owned(), s.to_owned()),
    }
}

#[derive(Default)]
struct Tally {
    pass: u64,
    fail: u64,
    skipped: u64,
}

struct Ctx<'a> {
    g: &'a Graph<GraphFile>,
    neo: &'a Neo4j,
    keys: HashMap<String, String>,
    /// Per node type: (ID, key) of every active node.
    by_type: HashMap<String, Vec<(String, String)>>,
    r: Rng,
    tally: BTreeMap<&'static str, Tally>,
    failures: Vec<Json>,
    verbose: bool,
}

/// What a case compares, or why it is skipped.
enum Outcome {
    Compare(Json, Json),
    Skip(String),
}

/// The Cypher condition that a link `r` from the expanding node `s` to `m` is followable
/// ([TRAV-1]); `KNOWS` is followed in every direction.
fn follow(r: &str, s: &str, m: &str, types: &[String], dir: &str, node_types: &[String]) -> String {
    let mut c = vec![];
    if !types.is_empty() {
        c.push(format!("type({r}) IN {}", list_lit(types)));
    }
    match dir {
        "out" => c.push(format!("(type({r}) = 'KNOWS' OR startNode({r}) = {s})")),
        "in" => c.push(format!("(type({r}) = 'KNOWS' OR endNode({r}) = {s})")),
        _ => {}
    }
    if !node_types.is_empty() {
        c.push(format!(
            "any(l IN labels({m}) WHERE l IN {})",
            list_lit(node_types)
        ));
    }
    if c.is_empty() {
        "true".into()
    } else {
        c.join(" AND ")
    }
}

impl Ctx<'_> {
    fn node(&mut self, ty: &str) -> (String, String) {
        let pool = &self.by_type[ty];
        pool[self.r.below(pool.len())].clone()
    }

    fn label_of(key: &str) -> &'static str {
        let table = key.split('/').next().unwrap_or_default();
        crate::schema::node_table(table).map_or("Person", |(_, t)| t.ty)
    }

    fn op(&self, name: &str, req: &Json) -> Result<Json> {
        let r = match name {
            "neighbors" => self.g.neighbors(req),
            "paths" => self.g.paths(req),
            "find" => self.g.find(req),
            _ => self.g.get(req),
        };
        r.map_err(|e| e.to_json().to_string().into())
    }

    fn run(
        &mut self,
        family: &'static str,
        case: Json,
        f: impl FnOnce(&mut Self) -> Result<Outcome>,
    ) {
        let outcome = f(self);
        let tally = self.tally.entry(family).or_default();
        match outcome {
            Ok(Outcome::Compare(a, b)) => {
                let (a, b) = (norm(&a), norm(&b));
                if a == b {
                    tally.pass += 1;
                } else {
                    tally.fail += 1;
                    let cut = |v: &Json| {
                        let s = v.to_string();
                        if s.len() > 600 {
                            format!("{}…", &s[..600])
                        } else {
                            s
                        }
                    };
                    self.failures.push(json!({
                        "family": family, "case": case, "mygraph": cut(&a), "neo4j": cut(&b),
                    }));
                }
            }
            Ok(Outcome::Skip(why)) => {
                tally.skipped += 1;
                if self.verbose {
                    eprintln!("skip {family}: {why}");
                }
            }
            Err(e) => {
                tally.fail += 1;
                self.failures
                    .push(json!({ "family": family, "case": case, "error": e.to_string() }));
            }
        }
    }

    /// The single group of a `find` without `group_by` ([OP-FIND-5]: there is one even when
    /// nothing matches); a missing group compares as `null` and fails the case.
    fn only_group(&self, res: &Json) -> Json {
        res["groups"][0].clone()
    }

    /// Node keys of a MyGraph `neighbors` result, or `None` when it was truncated.
    fn neighbor_keys(&self, res: &Json) -> Option<Vec<Json>> {
        if res["truncated"] == true {
            return None;
        }
        Some(
            res["nodes"]
                .as_array()?
                .iter()
                .filter_map(|n| self.keys.get(n["id"].as_str()?))
                .map(|k| json!(k))
                .collect(),
        )
    }

    fn random_filter(&mut self) -> (Vec<String>, String, Vec<String>) {
        let types = match self.r.below(3) {
            0 => vec![],
            1 => vec![self.r.pick(&LINK_TYPES).to_string()],
            _ => {
                let a = self.r.pick(&LINK_TYPES).to_string();
                let b = self.r.pick(&LINK_TYPES).to_string();
                if a == b { vec![a] } else { vec![a, b] }
            }
        };
        let dir = (*self.r.pick(&["both", "both", "out", "in"])).to_owned();
        let node_types = if self.r.chance(0.3) {
            vec![self.r.pick(&TYPES).to_string()]
        } else {
            vec![]
        };
        (types, dir, node_types)
    }

    fn neighbors_case(&mut self, depth: u32) {
        let ty = *self.r.pick(&TYPES);
        let (_, key) = self.node(ty);
        let (types, dir, node_types) = self.random_filter();
        let mut req = json!({ "start": [{ "key": key }], "depth": depth, "direction": dir,
                              "limit_per_node": 1000, "limit": 5000 });
        if !types.is_empty() {
            req["link_types"] = json!(types);
        }
        if !node_types.is_empty() {
            req["node_types"] = json!(node_types);
        }
        let label = Self::label_of(&key);
        let cypher = if depth == 1 {
            format!(
                "MATCH (s:{label} {{key: {k}}}) OPTIONAL MATCH (s)-[r]-(m) WHERE {f} \
                 WITH s, collect(DISTINCT m.key) AS ms, count(DISTINCT r) AS rs \
                 RETURN [s.key] + ms, rs",
                k = lit(&key),
                f = follow("r", "s", "m", &types, &dir, &node_types)
            )
        } else {
            format!(
                "MATCH (s:{label} {{key: {k}}}) OPTIONAL MATCH (s)-[r1]-(a) WHERE {f1} \
                 WITH s, collect(DISTINCT a) AS hop1 \
                 UNWIND (CASE WHEN hop1 = [] THEN [null] ELSE hop1 END) AS a \
                 OPTIONAL MATCH (a)-[r2]-(b) WHERE {f2} \
                 WITH s, hop1, collect(DISTINCT b.key) AS hop2 \
                 RETURN [s.key] + [x IN hop1 | x.key] + hop2",
                k = lit(&key),
                f1 = follow("r1", "s", "a", &types, &dir, &node_types),
                f2 = follow("r2", "a", "b", &types, &dir, &node_types)
            )
        };
        let family = if depth == 1 {
            "neighbors_1"
        } else {
            "neighbors_2"
        };
        let case = json!({ "request": req, "cypher": cypher });
        self.run(family, case, |c| {
            let res = c.op("neighbors", &req)?;
            let Some(mut keys) = c.neighbor_keys(&res) else {
                return Ok(Outcome::Skip("truncated".into()));
            };
            let row = c.neo.rows(&cypher)?.pop().ok_or("no row")?;
            let mut theirs: Vec<Json> = row[0].as_array().cloned().unwrap_or_default();
            theirs.sort_by_key(|x| x.to_string());
            theirs.dedup();
            keys.sort_by_key(|x| x.to_string());
            keys.dedup();
            if depth == 1 {
                let links = res["links"].as_array().map_or(0, Vec::len);
                Ok(Outcome::Compare(
                    json!([keys, links]),
                    json!([theirs, row[1]]),
                ))
            } else {
                Ok(Outcome::Compare(json!(keys), json!(theirs)))
            }
        });
    }

    fn paths_case(&mut self) {
        let pick_ty = |r: &mut Rng| *r.pick(&["Person", "Person", "Post", "Comment", "Forum"]);
        let (ta, tb) = (pick_ty(&mut self.r), pick_ty(&mut self.r));
        let ((_, a), (_, b)) = (self.node(ta), self.node(tb));
        if a == b {
            return;
        }
        let types: Vec<String> = self
            .r
            .pick(&PATH_LINK_SETS)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let all = self.r.chance(0.5);
        let req = json!({ "from": { "key": a }, "to": { "key": b }, "max_length": 4,
                          "link_types": types, "mode": if all { "all_shortest" } else { "shortest" } });
        let rel = format!(":{}", types.join("|"));
        let (la, lb) = (Self::label_of(&a), Self::label_of(&b));
        let cypher = if all {
            format!(
                "MATCH (a:{la} {{key: {}}}), (b:{lb} {{key: {}}}) \
                 OPTIONAL MATCH p = allShortestPaths((a)-[{rel}*..4]-(b)) \
                 WITH p LIMIT 101 RETURN min(length(p)), count(p)",
                lit(&a),
                lit(&b)
            )
        } else {
            format!(
                "MATCH (a:{la} {{key: {}}}), (b:{lb} {{key: {}}}) \
                 OPTIONAL MATCH p = shortestPath((a)-[{rel}*..4]-(b)) RETURN length(p), count(p)",
                lit(&a),
                lit(&b)
            )
        };
        let family = if all {
            "paths_all_shortest"
        } else {
            "paths_shortest"
        };
        let case = json!({ "request": req, "cypher": cypher });
        self.run(family, case, |c| {
            let res = c.op("paths", &req)?;
            let paths = res["paths"].as_array().ok_or("no paths")?;
            let len = paths.first().map_or(Json::Null, |p| p["length"].clone());
            let row = match c.neo.rows(&cypher) {
                Ok(mut rows) => rows.pop().ok_or("no row")?,
                Err(e) if e.to_string().contains("imeout") => {
                    return Ok(Outcome::Skip("neo4j timeout".into()));
                }
                Err(e) => return Err(e),
            };
            let n = row[1].as_u64().unwrap_or(0).min(100);
            Ok(Outcome::Compare(
                json!([len, paths.len()]),
                json!([row[0], n]),
            ))
        });
    }

    /// A random property condition on nodes of a random type, with the operand taken from a real
    /// node so filters are selective: MyGraph condition and Cypher expression over `n`.
    fn random_condition(&mut self, ty: &str) -> Result<Option<(Json, String)>> {
        let (id, _) = self.node(ty);
        let got = self.op(
            "get",
            &json!({ "refs": [id], "include": { "links": false, "support": false, "evidence": false } }),
        )?;
        let props = got["items"][0]["props"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut fields: Vec<(String, Json)> = props
            .into_iter()
            .map(|(k, v)| (format!("prop:{k}"), v))
            .collect();
        fields.push(("name".into(), got["items"][0]["name"].clone()));
        if let Some(l) = got["items"][0]["labels"].as_array().and_then(|l| l.first()) {
            fields.push(("labels".into(), l.clone()));
        }
        let (field, sample) = self.r.pick(&fields).clone();
        // The graph keeps LDBC's names (`naming: free`), so a property key is Neo4j's too.
        let col = field.strip_prefix("prop:").unwrap_or(&field).to_owned();
        let x = format!("n.{col}");
        let missing = format!("{x} IS NULL");
        Ok(Some(if field == "labels" {
            let l = sample.as_str().unwrap_or_default();
            (
                json!({ "field": "labels", "op": "eq", "value": l }),
                format!("n:{l}"),
            )
        } else if let Json::Array(items) = &sample {
            // A list: `eq` / `in` hold when some element matches; `ne` / `not_in` are their
            // negations, so no element matches ([OP-FIND-4]).
            let v = items
                .first()
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned();
            match self.r.below(5) {
                0 => (
                    json!({ "field": field, "op": "eq", "value": v }),
                    format!("{} IN {x}", lit(&v)),
                ),
                1 => (
                    json!({ "field": field, "op": "ne", "value": v }),
                    format!("({missing} OR NOT {} IN {x})", lit(&v)),
                ),
                2 => (
                    json!({ "field": field, "op": "not_in", "value": [v] }),
                    format!("({missing} OR NOT any(e IN {x} WHERE e IN [{}]))", lit(&v)),
                ),
                3 => (
                    json!({ "field": field, "op": "exists" }),
                    format!("{x} IS NOT NULL"),
                ),
                _ => (json!({ "field": field, "op": "missing" }), missing),
            }
        } else if let Some(s) = sample
            .get("$datetime")
            .or_else(|| sample.get("$date"))
            .and_then(Json::as_str)
        {
            // Temporal: the operand is the value cut to a year, month or day; compared as
            // intervals ([OP-FIND-4]).
            let is_dt = sample.get("$datetime").is_some();
            let cut = match self.r.below(3) {
                0 => &s[..4],
                1 => &s[..7],
                _ => &s[..10],
            };
            let (start, next) = period(cut);
            let t = |d: &str| {
                if is_dt {
                    format!("datetime('{d}T00:00:00Z')")
                } else {
                    format!("date('{d}')")
                }
            };
            // A cut value is a partial date even for a date-time property.
            let operand = json!({ "$date": cut });
            let eq = format!("({x} >= {} AND {x} < {})", t(&start), t(&next));
            let op = *self.r.pick(&["eq", "ne", "lt", "lte", "gt", "gte"]);
            let expr = match op {
                "eq" => eq,
                "ne" => format!("({missing} OR NOT {eq})"),
                "lt" => format!("{x} < {}", t(&start)),
                "lte" => format!("{x} < {}", t(&next)),
                "gt" => format!("{x} >= {}", t(&next)),
                _ => format!("{x} >= {}", t(&start)),
            };
            (json!({ "field": field, "op": op, "value": operand }), expr)
        } else if let Some(i) = sample.as_i64() {
            let op = *self
                .r
                .pick(&["eq", "ne", "lt", "lte", "gt", "gte", "between"]);
            if op == "between" {
                let hi = i + self.r.below(50) as i64;
                (
                    json!({ "field": field, "op": op, "value": [i, hi] }),
                    format!("({x} >= {i} AND {x} <= {hi})"),
                )
            } else {
                let cy = match op {
                    "eq" => format!("{x} = {i}"),
                    "ne" => format!("({missing} OR {x} <> {i})"),
                    "lt" => format!("{x} < {i}"),
                    "lte" => format!("{x} <= {i}"),
                    "gt" => format!("{x} > {i}"),
                    _ => format!("{x} >= {i}"),
                };
                (json!({ "field": field, "op": op, "value": i }), cy)
            }
        } else if let Some(s) = sample.as_str() {
            let v = lit(s);
            let op = *self
                .r
                .pick(&["eq", "ne", "lt", "gte", "in", "not_in", "exists", "missing"]);
            let cy = match op {
                "eq" => format!("{x} = {v}"),
                "ne" => format!("({missing} OR {x} <> {v})"),
                "lt" => format!("{x} < {v}"),
                "gte" => format!("{x} >= {v}"),
                "in" => format!("{x} IN [{v}, 'zz-none']"),
                "not_in" => format!("({missing} OR NOT {x} IN [{v}, 'zz-none'])"),
                "exists" => format!("{x} IS NOT NULL"),
                _ => missing,
            };
            let value = match op {
                "in" | "not_in" => json!([s, "zz-none"]),
                "exists" | "missing" => Json::Null,
                _ => json!(s),
            };
            let mut c = json!({ "field": field, "op": op });
            if !value.is_null() {
                c["value"] = value;
            }
            (c, cy)
        } else {
            return Ok(None);
        }))
    }

    fn find_count_case(&mut self) {
        let ty = *self.r.pick(&TYPES);
        let Ok(Some((cond, expr))) = self.random_condition(ty) else {
            self.tally.entry("find_count").or_default().skipped += 1;
            return;
        };
        let req = json!({ "target": "nodes",
            "where": { "all": [{ "field": "type", "op": "eq", "value": ty }, cond] },
            "aggregate": [{ "fn": "count" }] });
        let cypher = format!("MATCH (n:{ty}) WHERE {expr} RETURN count(n)");
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_count", case, |c| {
            let res = c.op("find", &req)?;
            let count = c.only_group(&res)["count"].clone();
            let row = c.neo.rows(&cypher)?.pop().ok_or("no row")?;
            Ok(Outcome::Compare(count, row[0].clone()))
        });
    }

    fn find_degree_case(&mut self) {
        let ty = *self
            .r
            .pick(&["Person", "Forum", "Post", "Comment", "Tag", "Place"]);
        let lt = *self.r.pick(&LINK_TYPES);
        let dir = *self.r.pick(&["both", "out", "in"]);
        let k = self.r.below(6) as i64;
        let op = *self.r.pick(&["gt", "eq", "lte"]);
        let pattern = match (dir, lt) {
            (_, "KNOWS") | ("both", _) => format!("(n)-[:{lt}]-()"),
            ("out", _) => format!("(n)-[:{lt}]->()"),
            _ => format!("(n)<-[:{lt}]-()"),
        };
        let cmp = match op {
            "gt" => ">",
            "eq" => "=",
            _ => "<=",
        };
        let field = format!("degree({lt}, {dir})");
        let req = json!({ "target": "nodes",
            "where": { "all": [{ "field": "type", "op": "eq", "value": ty },
                               { "field": field, "op": op, "value": k }] },
            "aggregate": [{ "fn": "count" }, { "fn": "max", "of": field }] });
        let cypher = format!(
            "MATCH (n:{ty}) WITH n, COUNT {{ {pattern} }} AS d WHERE d {cmp} {k} RETURN count(n), max(d)"
        );
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_degree", case, |c| {
            let res = c.op("find", &req)?;
            let g = &c.only_group(&res);
            let row = c.neo.rows(&cypher)?.pop().ok_or("no row")?;
            Ok(Outcome::Compare(
                json!([g["count"], g[format!("max({field})")]]),
                json!([row[0], row[1]]),
            ))
        });
    }

    /// Group-by with aggregates over one node type, from a fixed menu.
    fn find_group_cases(&mut self) {
        // (type, group_by expression, Cypher group expression)
        let groups: [(&str, &str, &str); 9] = [
            ("Person", "prop:browserUsed", "n.browserUsed"),
            ("Person", "prop:gender", "n.gender"),
            (
                "Person",
                "prop:birthday.decade",
                "(n.birthday.year / 10) * 10",
            ),
            ("Post", "prop:language", "n.language"),
            ("Post", "prop:creationDate.year", "n.creationDate.year"),
            ("Comment", "prop:creationDate.month", "n.creationDate.month"),
            ("Comment", "prop:browserUsed", "n.browserUsed"),
            ("Place", "labels", "[l IN labels(n) WHERE l <> 'Place']"),
            (
                "Organisation",
                "labels",
                "[l IN labels(n) WHERE l <> 'Organisation']",
            ),
        ];
        // (aggregate, output name, Cypher)
        let aggs: [(Json, &str, &str); 7] = [
            (json!({ "fn": "count" }), "count", "count(*)"),
            (
                json!({ "fn": "count", "of": "prop:length" }),
                "count(prop:length)",
                "count(n.length)",
            ),
            (
                json!({ "fn": "sum", "of": "prop:length" }),
                "sum(prop:length)",
                "CASE WHEN count(n.length) = 0 THEN null ELSE sum(n.length) END",
            ),
            (
                json!({ "fn": "avg", "of": "prop:length" }),
                "avg(prop:length)",
                "avg(n.length)",
            ),
            (
                json!({ "fn": "median", "of": "prop:length" }),
                "median(prop:length)",
                "CASE WHEN count(n.length) = 0 THEN null ELSE percentileCont(n.length, 0.5) END",
            ),
            (
                json!({ "fn": "min", "of": "prop:creationDate" }),
                "min(prop:creationDate)",
                "min(n.creationDate)",
            ),
            (
                json!({ "fn": "count_distinct", "of": "prop:locationIP" }),
                "count_distinct(prop:locationIP)",
                "count(DISTINCT n.locationIP)",
            ),
        ];
        for (ty, by, cy_by) in groups {
            let req = json!({ "target": "nodes",
                "where": { "field": "type", "op": "eq", "value": ty },
                "group_by": [by], "aggregate": aggs.iter().map(|a| a.0.clone()).collect::<Vec<_>>(),
                "limit": 1000 });
            let cypher = format!(
                "MATCH (n:{ty}) WITH n, {cy_by} AS g RETURN g, {}",
                aggs.iter().map(|a| a.2).collect::<Vec<_>>().join(", ")
            );
            let case = json!({ "request": req, "cypher": cypher });
            let names: Vec<&str> = aggs.iter().map(|a| a.1).collect();
            self.run("find_group", case, |c| {
                let res = c.op("find", &req)?;
                let ours = res["groups"]
                    .as_array()
                    .ok_or("no groups")?
                    .iter()
                    .map(|g| {
                        let mut row = vec![g["key"][by].clone()];
                        row.extend(names.iter().map(|n| g[*n].clone()));
                        Json::Array(row)
                    })
                    .collect();
                let theirs = c.neo.rows(&cypher)?.into_iter().map(Json::Array).collect();
                Ok(Outcome::Compare(sorted(ours), sorted(theirs)))
            });
        }
    }

    /// Links grouped by type and endpoint types, and filters on link properties.
    fn find_link_cases(&mut self) {
        let req = json!({ "target": "links", "group_by": ["type", "from.type", "to.type"], "limit": 1000 });
        let cypher = "MATCH (a)-[r]->(b) WITH type(r) AS t, \
            [l IN labels(a) WHERE l IN $T][0] AS ft, [l IN labels(b) WHERE l IN $T][0] AS tt \
            RETURN t, ft, tt, count(*)"
            .replace("$T", &list_lit(&TYPES.map(String::from)));
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_links", case, |c| {
            let res = c.op("find", &req)?;
            let ours = res["groups"]
                .as_array()
                .ok_or("no groups")?
                .iter()
                .map(|g| {
                    let k = &g["key"];
                    json!([k["type"], k["from.type"], k["to.type"], g["count"]])
                })
                .collect();
            let theirs = c.neo.rows(&cypher)?.into_iter().map(Json::Array).collect();
            Ok(Outcome::Compare(sorted(ours), sorted(theirs)))
        });
        for (lt, prop, value) in [
            ("HAS_MEMBER", "joinDate", "2011-06"),
            ("KNOWS", "creationDate", "2011"),
            ("LIKES", "creationDate", "2012-02-14"),
        ] {
            let (start, _) = period(value);
            for (op, cmp) in [("lt", "<"), ("gte", ">=")] {
                let req = json!({ "target": "links",
                    "where": { "all": [{ "field": "type", "op": "eq", "value": lt },
                                       { "field": format!("prop:{prop}"), "op": op, "value": { "$date": value } }] },
                    "aggregate": [{ "fn": "count" }] });
                // A datetime against a date operand: `lt` before its start, `gte` from its start.
                let cypher = format!(
                    "MATCH ()-[r:{lt}]->() WHERE r.{prop} {cmp} datetime('{start}T00:00:00Z') RETURN count(r)"
                );
                let case = json!({ "request": req, "cypher": cypher });
                self.run("find_links", case, |c| {
                    let res = c.op("find", &req)?;
                    let row = c.neo.rows(&cypher)?.pop().ok_or("no row")?;
                    Ok(Outcome::Compare(
                        c.only_group(&res)["count"].clone(),
                        row[0].clone(),
                    ))
                });
            }
        }
        let wf = self.r.below(15) as i64 + 2000;
        let req = json!({ "target": "links",
            "where": { "all": [{ "field": "type", "op": "eq", "value": "WORK_AT" },
                               { "field": "prop:workFrom", "op": "gte", "value": wf }] },
            "group_by": ["prop:workFrom"], "limit": 1000 });
        let cypher = format!(
            "MATCH ()-[r:WORK_AT]->() WHERE r.workFrom >= {wf} RETURN r.workFrom, count(*)"
        );
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_links", case, |c| {
            let res = c.op("find", &req)?;
            let ours = res["groups"]
                .as_array()
                .ok_or("no groups")?
                .iter()
                .map(|g| json!([g["key"]["prop:workFrom"], g["count"]]))
                .collect();
            let theirs = c.neo.rows(&cypher)?.into_iter().map(Json::Array).collect();
            Ok(Outcome::Compare(sorted(ours), sorted(theirs)))
        });
    }

    /// `find` with a `linked` condition: persons who know a given person, compared with Neo4j.
    fn find_linked_case(&mut self) {
        let (_, key) = self.node("Person");
        let req = json!({ "target": "nodes",
            "where": { "all": [{ "field": "type", "op": "eq", "value": "Person" },
                               { "linked": { "link_type": "KNOWS", "direction": "both", "node": { "key": key } } }] },
            "limit": 1000 });
        let cypher = format!(
            "MATCH (:Person {{key: {}}})-[:KNOWS]-(n:Person) RETURN collect(DISTINCT n.key)",
            lit(&key)
        );
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_linked", case, |c| {
            let res = c.op("find", &req)?;
            let ours: Vec<Json> = res["rows"]
                .as_array()
                .ok_or("no rows")?
                .iter()
                .filter_map(|r| c.keys.get(r["id"].as_str()?).map(|k| json!(k)))
                .collect();
            let row = c.neo.rows(&cypher)?.pop().ok_or("no row")?;
            Ok(Outcome::Compare(
                sorted(ours),
                sorted(row[0].as_array().cloned().unwrap_or_default()),
            ))
        });
    }
}

pub fn run(
    data: &std::path::Path,
    graph: &str,
    neo4j: &str,
    cases: usize,
    seed: u64,
    verbose: bool,
) -> Result<bool> {
    let file = GraphFile::open(data, graph)?;
    let policies = file.policies()?;
    let g = Graph::open(file, graph, policies)?;
    let neo = Neo4j::new(neo4j, "neo4j");
    let st = g.state();
    let mut keys = HashMap::new();
    let mut by_type: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (idx, n) in st.nodes.iter() {
        if let Some(k) = &n.key {
            let id = id_string(ObjectKind::Node, idx);
            keys.insert(id.clone(), k.clone());
            by_type
                .entry(n.type_.to_string())
                .or_default()
                .push((id, k.clone()));
        }
    }
    for ty in TYPES {
        if !by_type.contains_key(ty) {
            return Err(format!("no {ty} nodes in graph {graph}").into());
        }
    }
    let mut c = Ctx {
        g: &g,
        neo: &neo,
        keys,
        by_type,
        r: Rng(seed),
        tally: BTreeMap::new(),
        failures: Vec::new(),
        verbose,
    };
    c.find_group_cases();
    c.find_link_cases();
    for i in 0..cases {
        match i % 7 {
            0 | 1 => c.neighbors_case(1),
            2 => c.neighbors_case(2),
            3 => c.paths_case(),
            4 | 5 => c.find_count_case(),
            _ => {
                c.find_degree_case();
                c.find_linked_case();
            }
        }
    }
    let ok = c.failures.is_empty();
    let summary: Map<String, Json> = c
        .tally
        .iter()
        .map(|(k, t)| {
            (
                (*k).to_owned(),
                json!({ "pass": t.pass, "fail": t.fail, "skipped": t.skipped }),
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "graph": graph, "seed": seed, "summary": summary,
            "failures": c.failures.iter().take(30).collect::<Vec<_>>(),
            "failure_count": c.failures.len(),
        }))?
    );
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods() {
        assert_eq!(period("2011"), ("2011-01-01".into(), "2012-01-01".into()));
        assert_eq!(
            period("2011-12"),
            ("2011-12-01".into(), "2012-01-01".into())
        );
        assert_eq!(
            period("2012-02-29"),
            ("2012-02-29".into(), "2012-03-01".into())
        );
        assert_eq!(
            period("2011-02-28"),
            ("2011-02-28".into(), "2011-03-01".into())
        );
    }

    #[test]
    fn normal_forms() {
        assert_eq!(
            norm(&json!({ "$datetime": "2011-08-17T14:26:59.960Z" })),
            norm(&json!("2011-08-17T14:26:59.96Z"))
        );
        assert_eq!(
            norm(&json!("2011-08-17T14:26:59Z")),
            json!("2011-08-17T14:26:59.000Z")
        );
        assert_eq!(norm(&json!(140.0)), json!(140));
        assert_eq!(norm(&json!("Firefox")), json!("Firefox"));
    }
}
