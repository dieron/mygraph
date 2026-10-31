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
use mygraph_core::name_key::name_key;
use mygraph_store_sqlite::GraphFile;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// SplitMix64, as in `mygraph-bench`, so a seed fixes every case.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub(crate) fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
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
const PATH_LINK_SETS: [&[&str]; 7] = [
    &["KNOWS"],
    &["KNOWS", "STUDY_AT", "WORK_AT"],
    &["HAS_CREATOR", "REPLY_OF"],
    &["HAS_MEMBER", "KNOWS"],
    &["HAS_CREATOR", "LIKES", "KNOWS"],
    &["IS_LOCATED_IN", "IS_PART_OF"],
    &["HAS_TAG", "HAS_TYPE", "IS_SUBCLASS_OF"],
];
/// The LDBC schema's links as (type, from type, to type), so random steps follow links that
/// exist; `KNOWS` is undirected.
const LINK_ENDS: [(&str, &str, &str); 23] = [
    ("CONTAINER_OF", "Forum", "Post"),
    ("HAS_CREATOR", "Post", "Person"),
    ("HAS_CREATOR", "Comment", "Person"),
    ("HAS_INTEREST", "Person", "Tag"),
    ("HAS_MEMBER", "Forum", "Person"),
    ("HAS_MODERATOR", "Forum", "Person"),
    ("HAS_TAG", "Forum", "Tag"),
    ("HAS_TAG", "Post", "Tag"),
    ("HAS_TAG", "Comment", "Tag"),
    ("HAS_TYPE", "Tag", "TagClass"),
    ("IS_LOCATED_IN", "Person", "Place"),
    ("IS_LOCATED_IN", "Organisation", "Place"),
    ("IS_LOCATED_IN", "Post", "Place"),
    ("IS_LOCATED_IN", "Comment", "Place"),
    ("IS_PART_OF", "Place", "Place"),
    ("IS_SUBCLASS_OF", "TagClass", "TagClass"),
    ("KNOWS", "Person", "Person"),
    ("LIKES", "Person", "Post"),
    ("LIKES", "Person", "Comment"),
    ("REPLY_OF", "Comment", "Post"),
    ("REPLY_OF", "Comment", "Comment"),
    ("STUDY_AT", "Person", "Organisation"),
    ("WORK_AT", "Person", "Organisation"),
];
/// Link types with a validity (`schema::valid_from`), which `as_of` cases favour.
const DATED: [&str; 3] = ["KNOWS", "HAS_MEMBER", "WORK_AT"];
/// String fields `contains` / `starts_with` cases filter on: (type, field, Neo4j property).
const TEXT_FIELDS: [(&str, &str, &str); 8] = [
    ("Person", "name", "name"),
    ("Person", "prop:firstName", "firstName"),
    ("Person", "prop:lastName", "lastName"),
    ("Person", "prop:email", "email"),
    ("Tag", "name", "name"),
    ("Place", "name", "name"),
    ("Organisation", "name", "name"),
    ("Forum", "name", "name"),
];
/// Sort keys for `find` rows: (type, field expression, Cypher expression over `n`). Post `language`
/// and `imageFile` are each missing on some posts, which must sort last in both orders.
const SORT_FIELDS: [(&str, &str, &str); 17] = [
    ("Person", "name", "n.name"),
    ("Person", "prop:birthday", "n.birthday"),
    ("Person", "prop:creationDate", "n.creationDate"),
    ("Person", "prop:browserUsed", "n.browserUsed"),
    ("Person", "prop:lastName", "n.lastName"),
    ("Person", "degree(KNOWS)", "COUNT { (n)-[:KNOWS]-() }"),
    ("Forum", "prop:creationDate", "n.creationDate"),
    ("Forum", "name", "n.name"),
    (
        "Forum",
        "degree(HAS_MEMBER, out)",
        "COUNT { (n)-[:HAS_MEMBER]->() }",
    ),
    ("Tag", "name", "n.name"),
    ("Tag", "degree(HAS_TAG, in)", "COUNT { (n)<-[:HAS_TAG]-() }"),
    ("Organisation", "name", "n.name"),
    ("Place", "name", "n.name"),
    ("Place", "prop:url", "n.url"),
    ("Post", "prop:language", "n.language"),
    ("Post", "prop:imageFile", "n.imageFile"),
    ("Post", "prop:length", "n.length"),
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
        self.query(cypher, &json!({}))
    }

    /// The rows of one statement with parameters (so Neo4j can reuse its plan).
    pub(crate) fn query(&self, cypher: &str, params: &Json) -> Result<Vec<Vec<Json>>> {
        let body = json!({ "statements": [{ "statement": cypher, "parameters": params }] });
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

    /// Runs statements in one transaction; returns the nodes and relationships created.
    pub(crate) fn tx(&self, statements: &[(String, Json)]) -> Result<(u64, u64)> {
        let body = json!({ "statements": statements.iter().map(|(s, p)| json!({
            "statement": s, "parameters": p, "includeStats": true })).collect::<Vec<_>>() });
        let res: Json = self.agent.post(&self.url).send_json(body)?.into_json()?;
        if let Some(e) = res["errors"].as_array().and_then(|e| e.first()) {
            return Err(format!("neo4j: {}", e["message"]).into());
        }
        let sum = |k: &str| {
            res["results"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["stats"][k].as_u64())
                .sum()
        };
        Ok((sum("nodes_created"), sum("relationships_created")))
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
pub(crate) fn norm(v: &Json) -> Json {
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
    /// Per (type, property): (key, value) of every node, as Neo4j has them.
    text_values: HashMap<(String, String), Vec<(String, Json)>>,
}

/// What a case compares, or why it is skipped.
enum Outcome {
    Compare(Json, Json),
    Skip(String),
}

/// The Cypher condition that link `r` is valid at `as_of` ([TRAV-2]) under the loader's mapping
/// (`schema::valid_from`): `KNOWS` and `HAS_MEMBER` from their `creationDate` / `joinDate`,
/// `WORK_AT` from the start of its `workFrom` year; other links have no validity.
fn valid_at(r: &str, as_of: &str) -> String {
    let (_, next) = period(as_of);
    let end = format!("datetime('{next}T00:00:00Z')");
    format!(
        "CASE type({r}) WHEN 'KNOWS' THEN {r}.creationDate < {end}          WHEN 'HAS_MEMBER' THEN {r}.joinDate < {end}          WHEN 'WORK_AT' THEN {r}.workFrom <= {} ELSE true END",
        &as_of[..4]
    )
}

/// The Cypher condition that a link `r` from the expanding node `s` to `m` is followable
/// ([TRAV-1]); `KNOWS` is followed in every direction.
fn follow(
    r: &str,
    s: &str,
    m: &str,
    types: &[String],
    dir: &str,
    node_types: &[String],
    as_of: Option<&str>,
) -> String {
    let mut c = vec![];
    if let Some(t) = as_of {
        c.push(valid_at(r, t));
    }
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
                    if self.verbose {
                        eprintln!("{}", json!({ "family": family, "case": case, "result": a }));
                    }
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

    /// A random step from nodes of type `ty` — (link types, direction, node types) — and the
    /// type it mostly leads to. It follows a link the schema has from `ty`, usually in its
    /// direction, sometimes `both` or against it, sometimes with a second link type and a node
    /// type filter, so most steps reach something and some reach nothing. A `dated` step takes a
    /// link type with a validity when `ty` has one.
    fn step_from(&mut self, ty: &str, dated: bool) -> ((Vec<String>, String, Vec<String>), String) {
        let mut incident: Vec<_> = LINK_ENDS
            .iter()
            .filter(|e| e.1 == ty || e.2 == ty)
            .collect();
        if dated && incident.iter().any(|e| DATED.contains(&e.0)) {
            incident.retain(|e| DATED.contains(&e.0));
        }
        let &&(lt, from, to) = self.r.pick(&incident);
        let next = if from == ty { to } else { from };
        let mut types = vec![lt.to_owned()];
        if self.r.chance(0.3) {
            let other = self.r.pick(&incident).0;
            if other != lt {
                types.push(other.to_owned());
            }
        }
        let along = if from == ty { "out" } else { "in" };
        let against = if from == ty { "in" } else { "out" };
        let dir = if lt == "KNOWS" {
            *self.r.pick(&["both", "out", "in"])
        } else if self.r.chance(0.25) {
            "both"
        } else if self.r.chance(0.1) {
            against
        } else {
            along
        };
        let node_types = if self.r.chance(0.3) {
            vec![next.to_owned()]
        } else if self.r.chance(0.05) {
            vec![self.r.pick(&TYPES).to_string()]
        } else {
            vec![]
        };
        ((types, dir.to_owned(), node_types), next.to_owned())
    }

    fn neighbors_case(&mut self, depth: u32) {
        let as_of = self.r.chance(0.25).then(|| self.random_as_of());
        let ty = if as_of.is_some() {
            *self.r.pick(&["Person", "Person", "Forum", "Organisation"])
        } else {
            *self.r.pick(&TYPES)
        };
        let (_, key) = self.node(ty);
        let ((types, dir, node_types), _) = self.step_from(ty, as_of.is_some());
        let mut req = json!({ "start": [{ "key": key }], "depth": depth, "direction": dir,
                              "limit_per_node": 1000, "limit": 5000 });
        if let Some(t) = &as_of {
            req["as_of"] = json!({ "$date": t });
        }
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
                f = follow("r", "s", "m", &types, &dir, &node_types, as_of.as_deref())
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
                f1 = follow("r1", "s", "a", &types, &dir, &node_types, as_of.as_deref()),
                f2 = follow("r2", "a", "b", &types, &dir, &node_types, as_of.as_deref())
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

    /// A random `as_of` date: a year, month or day in 2010–2012, the years of LDBC's activity.
    fn random_as_of(&mut self) -> String {
        let y = 2010 + self.r.below(3);
        match self.r.below(3) {
            0 => format!("{y}"),
            1 => format!("{y}-{:02}", 1 + self.r.below(12)),
            _ => format!(
                "{y}-{:02}-{:02}",
                1 + self.r.below(12),
                1 + self.r.below(28)
            ),
        }
    }

    /// Where a random walk of up to `hops` followable links from `key` ends, so most `paths`
    /// cases have a path; `None` when it ends where it started.
    fn walk(
        &mut self,
        key: &str,
        types: &[String],
        dir: &str,
        as_of: Option<&str>,
        hops: usize,
    ) -> Result<Option<String>> {
        let mut at = key.to_owned();
        for _ in 0..hops {
            let mut req = json!({ "start": [{ "key": at }], "link_types": types, "direction": dir,
                                  "limit_per_node": 1000, "limit": 5000 });
            if let Some(t) = as_of {
                req["as_of"] = json!({ "$date": t });
            }
            let res = self.op("neighbors", &req)?;
            let next: Vec<String> = res["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|n| n["hop"] == 1)
                .filter_map(|n| self.keys.get(n["id"].as_str()?).cloned())
                .collect();
            if next.is_empty() {
                break;
            }
            at = self.r.pick(&next).clone();
        }
        Ok((at != key).then_some(at))
    }

    /// A MyGraph path as keys and link types in walking order: `[key, TYPE, key, …]`.
    fn our_path(&self, p: &Json) -> Json {
        let nodes = p["nodes"].as_array().cloned().unwrap_or_default();
        let links = p["links"].as_array().cloned().unwrap_or_default();
        let mut out = vec![];
        for (i, n) in nodes.iter().enumerate() {
            if i > 0 {
                out.push(links.get(i - 1).map_or(Json::Null, |l| l["type"].clone()));
            }
            let id = n["id"].as_str().unwrap_or_default();
            out.push(self.keys.get(id).map_or(Json::Null, |k| json!(k)));
        }
        Json::Array(out)
    }

    /// Paths from a Cypher query returning node keys and link types, in the form of `our_path`;
    /// `None` when Neo4j timed out.
    fn neo_paths(&self, cypher: &str) -> Result<Option<Vec<Json>>> {
        let rows = match self.neo.rows(cypher) {
            Ok(rows) => rows,
            Err(e) if e.to_string().contains("imeout") => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(
            rows.into_iter()
                .map(|row| {
                    let keys = row[0].as_array().cloned().unwrap_or_default();
                    let types = row[1].as_array().cloned().unwrap_or_default();
                    let mut out = vec![];
                    for (i, k) in keys.into_iter().enumerate() {
                        if i > 0 {
                            out.push(types.get(i - 1).cloned().unwrap_or(Json::Null));
                        }
                        out.push(k);
                    }
                    Json::Array(out)
                })
                .collect(),
        ))
    }

    /// `paths` in every mode, with a random direction and sometimes `as_of`. The Cypher side
    /// walks a quantified path pattern whose inner `WHERE` applies [TRAV-1] per step. Neo4j's
    /// shortest paths are shortest trails, which never repeat a node, so they are [TRAV-4] paths;
    /// for `k_shortest` Neo4j lists every path of each length, filtered to distinct nodes.
    fn paths_case(&mut self) {
        let as_of = self.r.chance(0.25).then(|| self.random_as_of());
        let sets: Vec<&[&str]> = PATH_LINK_SETS
            .into_iter()
            .filter(|set| as_of.is_none() || set.iter().any(|t| DATED.contains(t)))
            .collect();
        let types: Vec<String> = self.r.pick(&sets).iter().map(|s| s.to_string()).collect();
        let touched: Vec<&str> = LINK_ENDS
            .iter()
            .filter(|e| types.iter().any(|t| t == e.0))
            .flat_map(|e| [e.1, e.2])
            .collect();
        let ty = *self.r.pick(&touched);
        let (_, mut a) = self.node(ty);
        let dir = (*self.r.pick(&["both", "both", "out", "in"])).to_owned();
        let mode = *self.r.pick(&["shortest", "all_shortest", "k_shortest"]);
        let max_length: usize = if mode == "k_shortest" { 3 } else { 4 };
        let k = 1 + self.r.below(10);
        let hops = 1 + self.r.below(max_length);
        let mut walked = None;
        if self.r.chance(0.7) {
            for _ in 0..3 {
                walked = self
                    .walk(&a, &types, &dir, as_of.as_deref(), hops)
                    .ok()
                    .flatten();
                if walked.is_some() {
                    break;
                }
                a = self.node(ty).1;
            }
        }
        let b = match walked {
            Some(b) => b,
            None => {
                let t = *self.r.pick(&["Person", "Post", "Comment", "Forum"]);
                self.node(t).1
            }
        };
        if a == b {
            return;
        }
        let mut req = json!({ "from": { "key": a }, "to": { "key": b }, "max_length": max_length,
                              "link_types": types, "direction": dir, "mode": mode });
        if mode == "k_shortest" {
            req["k"] = json!(k);
        }
        if let Some(t) = &as_of {
            req["as_of"] = json!({ "$date": t });
        }
        let mut cond = vec![];
        match dir.as_str() {
            "out" => cond.push("(type(r) = 'KNOWS' OR startNode(r) = x)".to_owned()),
            "in" => cond.push("(type(r) = 'KNOWS' OR endNode(r) = x)".to_owned()),
            _ => {}
        }
        if let Some(t) = &as_of {
            cond.push(valid_at("r", t));
        }
        let step = format!(
            "((x)-[r:{}]-(y){})",
            types.join("|"),
            if cond.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", cond.join(" AND "))
            }
        );
        let ends = format!(
            "MATCH (a:{} {{key: {}}}), (b:{} {{key: {}}})",
            Self::label_of(&a),
            lit(&a),
            Self::label_of(&b),
            lit(&b)
        );
        let ret = "RETURN [n IN nodes(p) | n.key], [l IN relationships(p) | type(l)]";
        let shortest = format!(
            "{ends} MATCH p = ALL SHORTEST (a) {step}{{1,{max_length}}} (b) {ret} LIMIT 101"
        );
        let of_length = |l: usize| {
            format!(
                "{ends} MATCH p = (a) {step}{{{l}}} (b) \
                 WHERE all(i IN range(0, {l} - 1) WHERE NOT nodes(p)[i] IN nodes(p)[i + 1..]) \
                 {ret} LIMIT 2001"
            )
        };
        let family = match mode {
            "shortest" => "paths_shortest",
            "all_shortest" => "paths_all_shortest",
            _ => "paths_k_shortest",
        };
        let cypher = if mode == "k_shortest" {
            of_length(1)
        } else {
            shortest.clone()
        };
        let case = json!({ "request": req, "cypher": cypher });
        self.run(family, case, |c| {
            let res = c.op("paths", &req)?;
            if res["truncated"] == true {
                return Ok(Outcome::Skip("truncated".into()));
            }
            let ours: Vec<(usize, Json)> = res["paths"]
                .as_array()
                .ok_or("no paths")?
                .iter()
                .map(|p| (p["length"].as_u64().unwrap_or(0) as usize, c.our_path(p)))
                .collect();
            if mode != "k_shortest" {
                let Some(theirs) = c.neo_paths(&shortest)? else {
                    return Ok(Outcome::Skip("neo4j timeout".into()));
                };
                let len = theirs.first().map(|p| p.as_array().map_or(0, Vec::len) / 2);
                let our_len = ours.first().map(|p| p.0);
                return Ok(if mode == "shortest" {
                    // Which of the shortest paths comes first depends on internal numbers.
                    let known = ours
                        .first()
                        .is_none_or(|p| theirs.len() > 100 || theirs.contains(&p.1));
                    Outcome::Compare(
                        json!([ours.len(), our_len, known]),
                        json!([theirs.len().min(1), len, true]),
                    )
                } else if theirs.len() > 100 {
                    Outcome::Compare(json!([ours.len(), our_len]), json!([100, len]))
                } else {
                    Outcome::Compare(
                        sorted(ours.into_iter().map(|p| p.1).collect()),
                        sorted(theirs),
                    )
                });
            }
            // k_shortest: every path of the lengths before the last one returned, then as many
            // of the last length as are still wanted (which ones depends on internal numbers).
            let ascending = ours.windows(2).all(|w| w[0].0 <= w[1].0);
            let (mut need, mut last) = (k, 0);
            let (mut our_full, mut their_full) = (vec![], vec![]);
            let (mut our_cut, mut their_cut) = (Json::Null, Json::Null);
            for l in 1..=max_length {
                if need == 0 {
                    break;
                }
                last = l;
                let Some(theirs) = c.neo_paths(&of_length(l))? else {
                    return Ok(Outcome::Skip("neo4j timeout".into()));
                };
                let ours_l: Vec<Json> = ours
                    .iter()
                    .filter(|p| p.0 == l)
                    .map(|p| p.1.clone())
                    .collect();
                if theirs.len() <= need {
                    need -= theirs.len();
                    our_full.push(json!([l, sorted(ours_l)]));
                    their_full.push(json!([l, sorted(theirs)]));
                } else {
                    let known = ours_l
                        .iter()
                        .all(|p| theirs.len() > 2000 || theirs.contains(p));
                    our_cut = json!([l, ours_l.len(), known]);
                    their_cut = json!([l, need, true]);
                    need = 0;
                }
            }
            let longer = ours.iter().filter(|p| p.0 > last).count();
            Ok(Outcome::Compare(
                json!([ascending, our_full, our_cut, longer]),
                json!([true, their_full, their_cut, 0]),
            ))
        });
    }

    /// `neighbors` with 2–3 different `steps`, sometimes `as_of`. Neo4j expands hop by hop from
    /// the nodes first reached at the previous hop ([OP-NEIGH-2]), by element ID.
    fn neighbors_steps_case(&mut self) {
        let as_of = self.r.chance(0.3).then(|| self.random_as_of());
        let ty = if as_of.is_some() {
            *self.r.pick(&["Person", "Forum", "Organisation"])
        } else {
            *self.r.pick(&TYPES)
        };
        let (_, key) = self.node(ty);
        let n = 2 + self.r.below(2);
        let mut at = ty.to_owned();
        let mut steps = vec![];
        for i in 0..n {
            let (step, next) = self.step_from(&at, as_of.is_some() && i == 0);
            steps.push(step);
            at = next;
        }
        let step_json: Vec<Json> = steps
            .iter()
            .map(|(types, dir, node_types)| {
                let mut s = json!({ "direction": dir });
                if !types.is_empty() {
                    s["link_types"] = json!(types);
                }
                if !node_types.is_empty() {
                    s["node_types"] = json!(node_types);
                }
                s
            })
            .collect();
        let mut req = json!({ "start": [{ "key": key }], "steps": step_json,
                              "limit_per_node": 1000, "limit": 5000 });
        if let Some(t) = &as_of {
            req["as_of"] = json!({ "$date": t });
        }
        let label = Self::label_of(&key);
        let case = json!({ "request": req, "cypher": "one query per hop" });
        self.run("neighbors_steps", case, |c| {
            let res = c.op("neighbors", &req)?;
            let Some(mut ours) = c.neighbor_keys(&res) else {
                return Ok(Outcome::Skip("truncated".into()));
            };
            let links = res["links"].as_array().map_or(0, Vec::len);
            let start = c
                .neo
                .rows(&format!(
                    "MATCH (s:{label} {{key: {}}}) RETURN elementId(s)",
                    lit(&key)
                ))?
                .pop()
                .ok_or("no start")?;
            let start = start[0].as_str().ok_or("no element ID")?.to_owned();
            let mut seen = HashSet::from([start.clone()]);
            let mut theirs = vec![json!(key)];
            let mut rels = HashSet::new();
            let mut frontier = vec![start];
            for (types, dir, node_types) in &steps {
                if frontier.is_empty() {
                    break;
                }
                let cypher = format!(
                    "MATCH (a) WHERE elementId(a) IN {} MATCH (a)-[r]-(b) WHERE {} \
                     RETURN elementId(r), elementId(b), b.key",
                    list_lit(&frontier),
                    follow("r", "a", "b", types, dir, node_types, as_of.as_deref())
                );
                let mut next = vec![];
                for row in c.neo.rows(&cypher)? {
                    rels.insert(row[0].to_string());
                    let b = row[1].as_str().unwrap_or_default().to_owned();
                    if seen.insert(b.clone()) {
                        next.push(b);
                        theirs.push(row[2].clone());
                    }
                }
                frontier = next;
            }
            ours.sort_by_key(|x| x.to_string());
            Ok(Outcome::Compare(
                json!([ours, links]),
                json!([sorted(theirs), rels.len()]),
            ))
        });
    }

    /// Every row of a `find` request, page by page.
    fn all_rows(&self, req: &Json) -> Result<Vec<Json>> {
        let mut req = req.clone();
        req["limit"] = json!(1000);
        let mut rows = vec![];
        loop {
            let res = self.op("find", &req)?;
            rows.extend(res["rows"].as_array().cloned().unwrap_or_default());
            match res["next_cursor"].as_str() {
                Some(c) => req["cursor"] = json!(c),
                None => return Ok(rows),
            }
        }
    }

    /// (key, value of `prop`) of every node of type `ty` in Neo4j, cached.
    fn text_values(&mut self, ty: &str, prop: &str) -> Result<Vec<(String, Json)>> {
        let k = (ty.to_owned(), prop.to_owned());
        if !self.text_values.contains_key(&k) {
            let rows = self
                .neo
                .rows(&format!("MATCH (n:{ty}) RETURN n.key, n.{prop}"))?;
            let values = rows
                .into_iter()
                .map(|r| (r[0].as_str().unwrap_or_default().to_owned(), r[1].clone()))
                .collect();
            self.text_values.insert(k.clone(), values);
        }
        Ok(self.text_values[&k].clone())
    }

    /// `contains` / `starts_with` on a string or list field, sometimes under `not`, with an
    /// operand cut from a real value and its case changed. Cypher has no name key
    /// ([NORM-NAME-1]: case folding, accents removed), so the expected nodes are worked out here
    /// from Neo4j's values with the spec's name key, and MyGraph's are read page by page.
    fn find_text_case(&mut self) {
        let (ty, field, prop) = *self.r.pick(&TEXT_FIELDS);
        let op = *self.r.pick(&["contains", "starts_with"]);
        let negate = self.r.chance(0.25);
        let values = match self.text_values(ty, prop) {
            Ok(v) => v,
            Err(e) => {
                self.run("find_text", json!({ "type": ty, "field": field }), |_| {
                    Err(e)
                });
                return;
            }
        };
        let strings: Vec<&str> = values
            .iter()
            .flat_map(|(_, v)| match v {
                Json::Array(a) => a.iter().filter_map(Json::as_str).collect::<Vec<_>>(),
                _ => v.as_str().into_iter().collect(),
            })
            .collect();
        if strings.is_empty() {
            return;
        }
        let chars: Vec<char> = self.r.pick(&strings).chars().collect();
        let start = if op == "starts_with" {
            0
        } else {
            self.r.below(chars.len())
        };
        let len = 1 + self.r.below((chars.len() - start).min(5));
        let cut: String = chars[start..start + len].iter().collect();
        let operand = match self.r.below(3) {
            0 => cut.to_uppercase(),
            1 => cut.to_lowercase(),
            _ => cut,
        };
        let key = name_key(&operand);
        if key.is_empty() {
            return;
        }
        let holds = |s: &str| {
            let k = name_key(s);
            if op == "contains" {
                k.contains(&key)
            } else {
                k.starts_with(&key)
            }
        };
        let expected: Vec<Json> = values
            .iter()
            .filter(|(_, v)| {
                let m = match v {
                    Json::Array(a) => a.iter().filter_map(Json::as_str).any(holds),
                    _ => v.as_str().is_some_and(holds),
                };
                m != negate
            })
            .map(|(k, _)| json!(k))
            .collect();
        let mut cond = json!({ "field": field, "op": op, "value": operand });
        if negate {
            cond = json!({ "not": cond });
        }
        let req = json!({ "target": "nodes",
            "where": { "all": [{ "field": "type", "op": "eq", "value": ty }, cond] } });
        let case =
            json!({ "request": req, "cypher": format!("MATCH (n:{ty}) RETURN n.key, n.{prop}") });
        self.run("find_text", case, |c| {
            let ours: Vec<Json> = c
                .all_rows(&req)?
                .iter()
                .filter_map(|r| c.keys.get(r["id"].as_str()?).map(|k| json!(k)))
                .collect();
            Ok(Outcome::Compare(
                json!([ours.len(), sorted(ours)]),
                json!([expected.len(), sorted(expected)]),
            ))
        });
    }

    /// Rows sorted by one or two keys in either order, sometimes filtered, read in pages with
    /// `cursor`: the sequence of sort values must equal Cypher's `ORDER BY` with missing values
    /// last ([OP-FIND-7]). Ties are broken by internal number, so only values are compared, plus
    /// the total and that no row comes twice.
    fn find_sort_case(&mut self) {
        let ty = self.r.pick(&SORT_FIELDS).0;
        let menu: Vec<(&str, &str)> = SORT_FIELDS
            .iter()
            .filter(|f| f.0 == ty)
            .map(|f| (f.1, f.2))
            .collect();
        let first = *self.r.pick(&menu);
        let mut keys = vec![first];
        if menu.len() > 1 && self.r.chance(0.5) {
            let second = *self.r.pick(&menu);
            if second != first {
                keys.push(second);
            }
        }
        let desc: Vec<bool> = keys.iter().map(|_| self.r.chance(0.5)).collect();
        let (cond, expr) = match self.r.chance(0.5).then(|| self.random_condition(ty)) {
            Some(Ok(Some((cond, expr)))) => (Some(cond), expr),
            _ => (None, "true".to_owned()),
        };
        let limit = *self.r.pick(&[7, 50, 333, 1000]);
        let mut all = vec![json!({ "field": "type", "op": "eq", "value": ty })];
        all.extend(cond);
        let sort: Vec<Json> = keys
            .iter()
            .zip(&desc)
            .map(|(k, d)| json!({ "by": k.0, "order": if *d { "desc" } else { "asc" } }))
            .collect();
        let req = json!({ "target": "nodes", "where": { "all": all },
            "fields": keys.iter().map(|k| k.0).collect::<Vec<_>>(), "sort": sort, "limit": limit });
        let order: Vec<String> = desc
            .iter()
            .enumerate()
            .map(|(i, d)| format!("k{i} IS NULL, k{i}{}", if *d { " DESC" } else { "" }))
            .collect();
        let with: Vec<String> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{} AS k{i}", k.1))
            .collect();
        let ret: Vec<String> = (0..keys.len()).map(|i| format!("k{i}")).collect();
        let base = format!("MATCH (n:{ty}) WHERE {expr}");
        let cypher = format!(
            "{base} WITH n, {} ORDER BY {} RETURN {} LIMIT $N",
            with.join(", "),
            order.join(", "),
            ret.join(", ")
        );
        let case = json!({ "request": req, "cypher": cypher });
        self.run("find_sort", case, |c| {
            let mut req = req.clone();
            let (mut rows, mut total) = (vec![], Json::Null);
            for _ in 0..4 {
                let res = c.op("find", &req)?;
                total = res["total"].clone();
                rows.extend(res["rows"].as_array().cloned().unwrap_or_default());
                match res["next_cursor"].as_str() {
                    Some(cur) => req["cursor"] = json!(cur),
                    None => break,
                }
            }
            let ids: HashSet<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
            let repeated = rows.len() - ids.len();
            let ours: Vec<Json> = rows
                .iter()
                .map(|r| Json::Array(keys.iter().map(|k| r["values"][k.0].clone()).collect()))
                .collect();
            let count = c
                .neo
                .rows(&format!("{base} RETURN count(n)"))?
                .pop()
                .ok_or("no row")?;
            let theirs: Vec<Json> = c
                .neo
                .rows(&cypher.replace("$N", &rows.len().to_string()))?
                .into_iter()
                .map(Json::Array)
                .collect();
            Ok(Outcome::Compare(
                json!([total, repeated, ours]),
                json!([count[0], 0, theirs]),
            ))
        });
    }

    /// Group order ([OP-FIND-7]): by the `sort` key, then count descending, then group key,
    /// missing keys last; read in pages of a random size.
    fn find_group_order_cases(&mut self) {
        let groups: [(&str, &str, &str); 5] = [
            ("Person", "prop:browserUsed", "n.browserUsed"),
            ("Post", "prop:language", "n.language"),
            ("Comment", "prop:creationDate.month", "n.creationDate.month"),
            (
                "Person",
                "prop:birthday.decade",
                "(n.birthday.year / 10) * 10",
            ),
            (
                "Forum",
                "degree(HAS_MEMBER, out)",
                "COUNT { (n)-[:HAS_MEMBER]->() }",
            ),
        ];
        for (ty, by, cy) in groups {
            for sort in ["default", "key_asc", "key_desc", "count_asc"] {
                let (sort_json, order) = match sort {
                    "default" => (json!([]), "c DESC, g IS NULL, g"),
                    "key_asc" => (json!([{ "by": by }]), "g IS NULL, g"),
                    "key_desc" => (json!([{ "by": by, "order": "desc" }]), "g IS NULL, g DESC"),
                    _ => (
                        json!([{ "by": "count", "order": "asc" }]),
                        "c, g IS NULL, g",
                    ),
                };
                let limit = *self.r.pick(&[3, 25, 1000]);
                let req = json!({ "target": "nodes",
                    "where": { "field": "type", "op": "eq", "value": ty },
                    "group_by": [by], "sort": sort_json, "limit": limit });
                let cypher = format!(
                    "MATCH (n:{ty}) WITH {cy} AS g, count(*) AS c RETURN g, c ORDER BY {order}"
                );
                let case = json!({ "request": req, "cypher": cypher });
                self.run("find_group_order", case, |c| {
                    let mut req = req.clone();
                    let mut groups = vec![];
                    let mut total;
                    loop {
                        let res = c.op("find", &req)?;
                        total = Some(res["total"].clone());
                        groups.extend(
                            res["groups"]
                                .as_array()
                                .ok_or("no groups")?
                                .iter()
                                .map(|g| json!([g["key"][by], g["count"]])),
                        );
                        match res["next_cursor"].as_str() {
                            Some(cur) => req["cursor"] = json!(cur),
                            None => break,
                        }
                    }
                    let theirs: Vec<Json> =
                        c.neo.rows(&cypher)?.into_iter().map(Json::Array).collect();
                    Ok(Outcome::Compare(
                        json!([total, groups]),
                        json!([theirs.len(), theirs]),
                    ))
                });
            }
        }
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
        text_values: HashMap::new(),
    };
    c.find_group_cases();
    c.find_group_order_cases();
    c.find_link_cases();
    for i in 0..cases {
        match i % 10 {
            0 | 1 => c.neighbors_case(1),
            2 => c.neighbors_case(2),
            3 => c.neighbors_steps_case(),
            4 => c.paths_case(),
            5 | 6 => c.find_count_case(),
            7 => {
                c.find_degree_case();
                c.find_linked_case();
            }
            8 => c.find_text_case(),
            _ => c.find_sort_case(),
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
