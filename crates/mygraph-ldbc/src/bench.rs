//! The workload driver (docs/11-benchmarks.md §2): the short reads IS1–IS7 of LDBC SNB
//! Interactive v1, an IC1-style and the IC13 read, and the inserts of the update streams, run
//! against MyGraph and Neo4j with the same parameters. Each query is timed on both (p50 / p95 /
//! p99) and the two answers are compared, so both did the same work.
//!
//! MyGraph answers through its operations, as an agent would: a query may take several calls,
//! and the driver counts them. It runs in this process or over the REST API (`--mygraph-url`).
//! Neo4j answers one Cypher statement per query, with parameters, over its HTTP API. Parameters
//! are drawn from Neo4j with a seed, so both modes use the same ones.

use crate::archive::Result;
use crate::diff::{Neo4j, Rng, norm};
use crate::updates::{self, Insert, OP_NAMES};
use mygraph_core::engine::Graph;
use mygraph_store_sqlite::GraphFile;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

pub const QUERIES: [&str; 9] = [
    "IS1", "IS2", "IS3", "IS4", "IS5", "IS6", "IS7", "IC1", "IC13",
];

pub struct Options {
    pub data: Option<PathBuf>,
    pub graph: String,
    pub neo4j: String,
    pub mygraph_url: Option<String>,
    pub queries: Vec<String>,
    pub runs: usize,
    pub warmup: usize,
    pub seed: u64,
    pub updates: usize,
    pub archive: Option<PathBuf>,
}

pub enum MyGraph {
    Local(Box<Graph<GraphFile>>),
    Http { base: String, agent: ureq::Agent },
}

impl MyGraph {
    fn call(&mut self, op: &str, req: &Json) -> Result<Json> {
        match self {
            MyGraph::Local(g) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as i64;
                let r = match op {
                    "get" => g.get(req),
                    "find" => g.find(req),
                    "neighbors" => g.neighbors(req),
                    "paths" => g.paths(req),
                    "write" => g.write("ldbc-driver", req, now),
                    _ => return Err(format!("unknown operation {op}").into()),
                };
                r.map_err(|e| e.to_json().to_string().into())
            }
            MyGraph::Http { base, agent } => {
                match agent.post(&format!("{base}/{op}")).send_json(req.clone()) {
                    Ok(r) => Ok(r.into_json()?),
                    Err(ureq::Error::Status(code, r)) => {
                        Err(format!("HTTP {code}: {}", r.into_string()?).into())
                    }
                    Err(e) => Err(e.into()),
                }
            }
        }
    }
}

struct Driver {
    mg: MyGraph,
    neo: Neo4j,
    /// MyGraph calls made by the current query.
    calls: u32,
    /// Per operation: total milliseconds and calls, over the current query's runs.
    by_op: BTreeMap<String, (f64, u32)>,
}

/// The Neo4j label of a key's table (`comment/12` → `Comment`).
fn label(key: &str) -> &'static str {
    let table = key.split('/').next().unwrap_or_default();
    crate::schema::node_table(table).map_or("Person", |(_, t)| t.ty)
}

/// The nodes at the other end of an item's links of one type (and direction).
fn others(item: &Json, ty: &str, dir: Option<&str>) -> Vec<Json> {
    item["links"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["type"] == ty && dir.is_none_or(|d| l["direction"] == d))
        .map(|l| l["other"].clone())
        .collect()
}

fn s(v: &Json) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

/// `content`, or `imageFile` for a post that is an image.
fn content(props: &Json) -> Json {
    match &props["content"] {
        Json::Null => props["imageFile"].clone(),
        c => c.clone(),
    }
}

/// Start IDs in order, and per start ID the (ID, name) of the post its thread starts with.
type Roots = (Vec<String>, HashMap<String, (String, String)>);

/// The end of the chain of `parent` links from `id`.
fn top_of(parent: &HashMap<String, String>, id: &str) -> String {
    let mut id = id;
    while let Some(p) = parent.get(id) {
        id = p;
    }
    id.to_owned()
}

fn sorted(mut v: Vec<Json>) -> Json {
    v.sort_by_key(|x| x.to_string());
    Json::Array(v)
}

impl Driver {
    fn op(&mut self, name: &str, req: Json) -> Result<Json> {
        self.calls += 1;
        let (ms, r) = timed(|| self.mg.call(name, &req));
        let e = self.by_op.entry(name.to_owned()).or_default();
        e.0 += ms;
        e.1 += 1;
        r
    }

    /// `get` in chunks of 100, with the links of `links` types (up to 500 each node).
    fn get(&mut self, refs: Vec<Json>, links: &[&str]) -> Result<Vec<Json>> {
        let include = if links.is_empty() {
            json!({ "links": false, "support": false, "evidence": false })
        } else {
            json!({ "links": { "types": links, "limit": 500 }, "support": false, "evidence": false })
        };
        let mut out = vec![];
        for chunk in refs.chunks(100) {
            let res = self.op("get", json!({ "refs": chunk, "include": include }))?;
            for it in res["items"].as_array().ok_or("no items")? {
                if it.get("error").is_some() || it["links"]["truncated"] == true {
                    return Err(format!("get: {it}").into());
                }
                out.push(it.clone());
            }
        }
        Ok(out)
    }

    /// For each start (references to messages), the post at the top of its `REPLY_OF` chain.
    fn roots(&mut self, starts: Vec<Json>) -> Result<Roots> {
        let mut parent: HashMap<String, String> = HashMap::new();
        let mut info: HashMap<String, (String, String)> = HashMap::new();
        let mut ids = vec![];
        let mut frontier = starts;
        loop {
            let first = ids.is_empty();
            for chunk in frontier.chunks(50) {
                let res = self.op(
                    "neighbors",
                    json!({ "start": chunk, "link_types": ["REPLY_OF"], "direction": "out",
                            "depth": 5, "limit_per_node": 1, "limit": 5000 }),
                )?;
                for n in res["nodes"].as_array().ok_or("no nodes")? {
                    if first && n["hop"] == 0 {
                        ids.push(s(&n["id"]));
                    }
                    info.insert(s(&n["id"]), (s(&n["type"]), s(&n["name"])));
                }
                for l in res["links"].as_array().ok_or("no links")? {
                    parent.insert(s(&l["from"]), s(&l["to"]));
                }
            }
            let top = |id: &String| top_of(&parent, id);
            let open: HashSet<String> = ids
                .iter()
                .map(top)
                .filter(|t| info.get(t).is_some_and(|i| i.0 != "Post"))
                .collect();
            if open.is_empty() {
                let roots = ids
                    .iter()
                    .map(|i| {
                        let t = top(i);
                        (i.clone(), (t.clone(), info[&t].1.clone()))
                    })
                    .collect();
                return Ok((ids, roots));
            }
            let next: Vec<Json> = open.iter().map(|i| json!(i)).collect();
            if next == frontier {
                return Err("a reply chain ends without a post".into());
            }
            frontier = next;
        }
    }

    fn mygraph(&mut self, q: &str, p: &Params) -> Result<Json> {
        let key = |k: &str| json!({ "key": k });
        Ok(match q {
            "IS1" => {
                let it = &self.get(vec![key(&p.person)], &["IS_LOCATED_IN"])?[0];
                let pr = &it["props"];
                let city = others(it, "IS_LOCATED_IN", Some("out"));
                json!([
                    pr["firstName"],
                    pr["lastName"],
                    pr["birthday"],
                    pr["locationIP"],
                    pr["browserUsed"],
                    city.first().map(|c| c["name"].clone()),
                    pr["gender"],
                    pr["creationDate"]
                ])
            }
            "IS2" => {
                let res = self.op("find", json!({ "target": "nodes",
                    "where": { "all": [
                        { "field": "type", "op": "in", "value": ["Post", "Comment"] },
                        { "linked": { "link_type": "HAS_CREATOR", "direction": "out", "node": key(&p.person) } }] },
                    "fields": ["prop:creationDate", "prop:content", "prop:imageFile"],
                    "sort": [{ "by": "prop:creationDate", "order": "desc" }], "limit": 10 }))?;
                let rows = res["rows"].as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    return Ok(json!([]));
                }
                let (_, roots) = self.roots(rows.iter().map(|r| r["id"].clone()).collect())?;
                let posts: Vec<String> = roots
                    .values()
                    .map(|r| r.0.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let creators: HashMap<String, Json> = self
                    .get(posts.iter().map(|i| json!(i)).collect(), &["HAS_CREATOR"])?
                    .iter()
                    .map(|it| {
                        let c = others(it, "HAS_CREATOR", Some("out"));
                        (
                            s(&it["id"]),
                            c.first().map_or(Json::Null, |c| c["name"].clone()),
                        )
                    })
                    .collect();
                Json::Array(
                    rows.iter()
                        .map(|r| {
                            let v = &r["values"];
                            let root = &roots[&s(&r["id"])];
                            json!([r["name"],
                                   content(&json!({ "content": v["prop:content"], "imageFile": v["prop:imageFile"] })),
                                   v["prop:creationDate"], root.1, creators[&root.0]])
                        })
                        .collect(),
                )
            }
            "IS3" => {
                let it = &self.get(vec![key(&p.person)], &["KNOWS"])?[0];
                let links: Vec<(Json, Json)> = it["links"]["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|l| (l["id"].clone(), l["other"]["name"].clone()))
                    .collect();
                let dates = self.get(links.iter().map(|l| l.0.clone()).collect(), &[])?;
                sorted(
                    links
                        .iter()
                        .zip(&dates)
                        .map(|((_, name), l)| json!([name, l["props"]["creationDate"]]))
                        .collect(),
                )
            }
            "IS4" => {
                let it = &self.get(vec![key(&p.message)], &[])?[0];
                json!([it["props"]["creationDate"], content(&it["props"])])
            }
            "IS5" => {
                let it = &self.get(vec![key(&p.message)], &["HAS_CREATOR"])?[0];
                json!(
                    others(it, "HAS_CREATOR", Some("out"))
                        .iter()
                        .map(|c| c["name"].clone())
                        .collect::<Vec<_>>()
                )
            }
            "IS6" => {
                let (ids, roots) = self.roots(vec![key(&p.message)])?;
                let post = &roots[ids.first().ok_or("message not found")?].0;
                let res = self.op(
                    "neighbors",
                    json!({ "start": [post], "steps": [
                    { "link_types": ["CONTAINER_OF"], "direction": "in" },
                    { "link_types": ["HAS_MODERATOR"], "direction": "out" }] }),
                )?;
                let at = |hop: u64| {
                    res["nodes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|n| n["hop"] == hop)
                        .map(|n| n["name"].clone())
                        .collect::<Vec<_>>()
                };
                match (at(1).as_slice(), at(2).as_slice()) {
                    ([f], [m]) => json!([[f, m]]),
                    _ => json!([]),
                }
            }
            "IS7" => {
                let it = &self.get(vec![key(&p.message)], &["REPLY_OF", "HAS_CREATOR"])?[0];
                let replies = others(it, "REPLY_OF", Some("in"));
                if replies.is_empty() {
                    return Ok(json!([]));
                }
                let author = others(it, "HAS_CREATOR", Some("out"));
                let author = author.first().ok_or("no author")?["id"].clone();
                let items = self.get(
                    replies.iter().map(|r| r["id"].clone()).collect(),
                    &["HAS_CREATOR"],
                )?;
                let friends: HashSet<String> =
                    others(&self.get(vec![author], &["KNOWS"])?[0], "KNOWS", None)
                        .iter()
                        .map(|f| s(&f["id"]))
                        .collect();
                sorted(
                    items
                        .iter()
                        .map(|r| {
                            let c = others(r, "HAS_CREATOR", Some("out"));
                            let c = c.first().cloned().unwrap_or(Json::Null);
                            json!([
                                r["name"],
                                r["props"]["content"],
                                r["props"]["creationDate"],
                                c["name"],
                                friends.contains(&s(&c["id"]))
                            ])
                        })
                        .collect(),
                )
            }
            "IC1" => {
                let res = self.op("find", json!({ "target": "nodes",
                    "where": { "all": [{ "field": "type", "op": "eq", "value": "Person" },
                                       { "field": "prop:firstName", "op": "eq", "value": p.first_name }] },
                    "fields": ["prop:lastName"], "limit": 1000 }))?;
                let nb = self.op(
                    "neighbors",
                    json!({ "start": [key(&p.person)], "link_types": ["KNOWS"],
                    "depth": 3, "limit_per_node": 1000, "limit": 5000 }),
                )?;
                if nb["truncated"] == true {
                    return Err("neighbors truncated".into());
                }
                let hop: HashMap<String, u64> = nb["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|n| n["hop"] != 0)
                    .map(|n| (s(&n["id"]), n["hop"].as_u64().unwrap_or(0)))
                    .collect();
                let mut found: Vec<(u64, String, String)> = res["rows"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| {
                        let id = s(&r["id"]);
                        Some((*hop.get(&id)?, s(&r["values"]["prop:lastName"]), id))
                    })
                    .collect();
                found.sort();
                json!(
                    found
                        .iter()
                        .take(20)
                        .map(|f| json!([f.0, f.1]))
                        .collect::<Vec<_>>()
                )
            }
            "IC13" => {
                let res = self.op(
                    "paths",
                    json!({ "from": key(&p.person), "to": key(&p.other),
                    "link_types": ["KNOWS"], "max_length": 10 }),
                )?;
                json!(res["paths"][0]["length"].as_i64().unwrap_or(-1))
            }
            _ => return Err(format!("unknown query {q}").into()),
        })
    }

    fn neo4j(&self, q: &str, p: &Params) -> Result<Json> {
        let ml = label(&p.message);
        let (cypher, params) = match q {
            "IS1" => (
                "MATCH (n:Person {key: $key})-[:IS_LOCATED_IN]->(c:Place) RETURN n.firstName, n.lastName, \
                 n.birthday, n.locationIP, n.browserUsed, c.name, n.gender, n.creationDate"
                    .to_owned(),
                json!({ "key": p.person }),
            ),
            "IS2" => (
                "MATCH (:Person {key: $key})<-[:HAS_CREATOR]-(m) WITH m ORDER BY m.creationDate DESC LIMIT 10 \
                 MATCH (m)-[:REPLY_OF*0..]->(p:Post)-[:HAS_CREATOR]->(c:Person) \
                 RETURN m.name, coalesce(m.content, m.imageFile), m.creationDate, p.name, c.name \
                 ORDER BY m.creationDate DESC"
                    .to_owned(),
                json!({ "key": p.person }),
            ),
            "IS3" => (
                "MATCH (n:Person {key: $key})-[r:KNOWS]-(f:Person) RETURN f.name, r.creationDate".to_owned(),
                json!({ "key": p.person }),
            ),
            "IS4" => (
                format!("MATCH (m:{ml} {{key: $key}}) RETURN m.creationDate, coalesce(m.content, m.imageFile)"),
                json!({ "key": p.message }),
            ),
            "IS5" => (
                format!("MATCH (m:{ml} {{key: $key}})-[:HAS_CREATOR]->(p:Person) RETURN p.name"),
                json!({ "key": p.message }),
            ),
            "IS6" => (
                format!(
                    "MATCH (m:{ml} {{key: $key}})-[:REPLY_OF*0..]->(:Post)<-[:CONTAINER_OF]-(f:Forum)\
                     -[:HAS_MODERATOR]->(p:Person) RETURN f.name, p.name"
                ),
                json!({ "key": p.message }),
            ),
            "IS7" => (
                format!(
                    "MATCH (m:{ml} {{key: $key}})<-[:REPLY_OF]-(c:Comment)-[:HAS_CREATOR]->(p:Person) \
                     OPTIONAL MATCH (m)-[:HAS_CREATOR]->(:Person)-[r:KNOWS]-(p) \
                     RETURN c.name, c.content, c.creationDate, p.name, r IS NOT NULL"
                ),
                json!({ "key": p.message }),
            ),
            "IC1" => (
                "MATCH (p:Person {key: $key}), (f:Person {firstName: $name}) WHERE f <> p \
                 MATCH path = shortestPath((p)-[:KNOWS*1..3]-(f)) \
                 RETURN length(path), f.lastName ORDER BY length(path), f.lastName, f.key LIMIT 20"
                    .to_owned(),
                json!({ "key": p.person, "name": p.first_name }),
            ),
            "IC13" => (
                "MATCH (a:Person {key: $a}), (b:Person {key: $b}) \
                 OPTIONAL MATCH p = shortestPath((a)-[:KNOWS*..10]-(b)) \
                 RETURN CASE WHEN p IS NULL THEN -1 ELSE length(p) END"
                    .to_owned(),
                json!({ "a": p.person, "b": p.other }),
            ),
            _ => return Err(format!("unknown query {q}").into()),
        };
        let rows: Vec<Json> = self
            .neo
            .query(&cypher, &params)?
            .into_iter()
            .map(Json::Array)
            .collect();
        Ok(match q {
            "IS1" | "IS4" => rows.into_iter().next().unwrap_or(Json::Null),
            "IS5" => json!(rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>()),
            "IS3" | "IS7" => sorted(rows),
            "IC13" => rows[0][0].clone(),
            _ => Json::Array(rows),
        })
    }
}

/// The parameters of one query run; each query uses some of them.
struct Params {
    person: String,
    other: String,
    first_name: String,
    message: String,
}

#[derive(Default)]
struct Stat {
    ms: Vec<f64>,
    calls: Vec<u32>,
    errors: u64,
}

impl Stat {
    fn summary(&self) -> Json {
        let mut v = self.ms.clone();
        v.sort_by(f64::total_cmp);
        let pct = |q: f64| {
            if v.is_empty() {
                return Json::Null;
            }
            let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
            json!((v[i] * 1000.0).round() / 1000.0)
        };
        let mean = (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
        let mut out = json!({ "n": v.len(), "p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99),
                              "mean": mean.map(|m| (m * 1000.0).round() / 1000.0), "errors": self.errors });
        if !self.calls.is_empty() {
            out["calls"] = json!(self.calls.iter().sum::<u32>() as f64 / self.calls.len() as f64);
        }
        out
    }
}

fn timed<T>(f: impl FnOnce() -> Result<T>) -> (f64, Result<T>) {
    let t = Instant::now();
    let r = f();
    (t.elapsed().as_secs_f64() * 1000.0, r)
}

pub fn run(o: &Options) -> Result<bool> {
    let mg = match &o.mygraph_url {
        Some(url) => MyGraph::Http {
            base: format!("{}/v1/graphs/{}", url.trim_end_matches('/'), o.graph),
            agent: ureq::AgentBuilder::new().build(),
        },
        None => {
            let data = o
                .data
                .as_ref()
                .ok_or("--data or --mygraph-url is required")?;
            let file = GraphFile::open(data, &o.graph)?;
            let policies = file.policies()?;
            MyGraph::Local(Box::new(Graph::open(file, &o.graph, policies)?))
        }
    };
    let mut d = Driver {
        mg,
        neo: Neo4j::new(&o.neo4j, "neo4j"),
        calls: 0,
        by_op: BTreeMap::new(),
    };
    let people: Vec<(String, String)> = d
        .neo
        .query(
            "MATCH (p:Person) RETURN p.key, p.firstName ORDER BY p.key",
            &json!({}),
        )?
        .into_iter()
        .map(|r| (s(&r[0]), s(&r[1])))
        .collect();
    let messages: Vec<String> = d
        .neo
        .query(
            "MATCH (m:Post) RETURN m.key AS k UNION ALL MATCH (m:Comment) RETURN m.key AS k",
            &json!({}),
        )?
        .into_iter()
        .map(|r| s(&r[0]))
        .collect();
    let mut rng = Rng(o.seed);

    // Transport floor: the cheapest round trip on each side.
    let mut floor = BTreeMap::new();
    let mut neo_floor = Stat::default();
    for _ in 0..o.runs {
        let (ms, r) = timed(|| d.neo.query("RETURN 1", &json!({})));
        r?;
        neo_floor.ms.push(ms);
    }
    floor.insert("neo4j RETURN 1", neo_floor.summary());

    let mut report = BTreeMap::new();
    let mut ok = true;
    for q in &o.queries {
        let (mut ours, mut theirs) = (Stat::default(), Stat::default());
        let mut mismatches = vec![];
        let mut mismatch_count = 0u64;
        d.by_op.clear();
        for i in 0..o.warmup + o.runs {
            let person = rng.pick(&people).0.clone();
            let mut other = rng.pick(&people).0.clone();
            if other == person {
                other = rng.pick(&people).0.clone();
            }
            let p = Params {
                person,
                other,
                first_name: rng.pick(&people).1.clone(),
                message: rng.pick(&messages).clone(),
            };
            d.calls = 0;
            // Alternate which side goes first, so neither always meets a warmer cache.
            let (a, b) = if i % 2 == 0 {
                let a = timed(|| d.mygraph(q, &p));
                (a, timed(|| d.neo4j(q, &p)))
            } else {
                let b = timed(|| d.neo4j(q, &p));
                (timed(|| d.mygraph(q, &p)), b)
            };
            let measured = i >= o.warmup;
            match (&a.1, &b.1) {
                (Ok(x), Ok(y)) => {
                    if measured {
                        ours.ms.push(a.0);
                        ours.calls.push(d.calls);
                        theirs.ms.push(b.0);
                    }
                    if norm(x) != norm(y) {
                        mismatch_count += 1;
                        if mismatches.len() < 5 {
                            mismatches.push(
                                json!({ "params": [p.person, p.other, p.first_name, p.message],
                                                    "mygraph": norm(x), "neo4j": norm(y) }),
                            );
                        }
                    }
                }
                _ => {
                    ours.errors += u64::from(a.1.is_err());
                    theirs.errors += u64::from(b.1.is_err());
                    if mismatches.len() < 5 {
                        mismatches.push(
                            json!({ "params": [p.person, p.other, p.first_name, p.message],
                            "mygraph_error": a.1.as_ref().err().map(|e| e.to_string()),
                            "neo4j_error": b.1.as_ref().err().map(|e| e.to_string()) }),
                        );
                    }
                }
            }
        }
        ok &= mismatch_count == 0 && ours.errors == 0 && theirs.errors == 0;
        let mut summary = ours.summary();
        summary["ms_per_call"] = d
            .by_op
            .iter()
            .map(|(k, (ms, n))| {
                (
                    k.clone(),
                    json!((ms / f64::from(*n) * 1000.0).round() / 1000.0),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        report.insert(
            q.clone(),
            json!({ "mygraph": summary, "neo4j": theirs.summary(),
                    "mismatches": mismatch_count, "examples": mismatches }),
        );
    }

    let mut inserts = BTreeMap::new();
    if o.updates > 0 {
        let archive = o.archive.as_ref().ok_or("--updates needs --archive")?;
        let events = updates::read(archive, o.updates)?;
        let mut stats: BTreeMap<u8, (Stat, Stat)> = BTreeMap::new();
        let (mut ours_created, mut theirs_created) = ((0u64, 0u64), (0u64, 0u64));
        let mut errors = vec![];
        for e in &events {
            let ins = Insert::from_event(e)?;
            let (ms, r) = timed(|| d.mg.call("write", &ins.write_request()));
            let st = stats.entry(ins.op).or_default();
            match r {
                Ok(res) => {
                    st.0.ms.push(ms);
                    let c = &res["counts"]["created"];
                    ours_created.0 += c["node"].as_u64().unwrap_or(0);
                    ours_created.1 += c["link"].as_u64().unwrap_or(0);
                }
                Err(err) => {
                    st.0.errors += 1;
                    if errors.len() < 5 {
                        errors.push(json!({ "op": ins.op, "mygraph": err.to_string() }));
                    }
                }
            }
            let (ms, r) = timed(|| d.neo.tx(&ins.statements()));
            match r {
                Ok((n, l)) => {
                    st.1.ms.push(ms);
                    theirs_created.0 += n;
                    theirs_created.1 += l;
                }
                Err(err) => {
                    st.1.errors += 1;
                    if errors.len() < 5 {
                        errors.push(json!({ "op": ins.op, "neo4j": err.to_string() }));
                    }
                }
            }
        }
        for (op, (a, b)) in &stats {
            inserts.insert(
                OP_NAMES[usize::from(*op) - 1].to_owned(),
                json!({ "mygraph": a.summary(), "neo4j": b.summary() }),
            );
        }
        ok &= errors.is_empty() && ours_created == theirs_created;
        inserts.insert(
            "created".to_owned(),
            json!({ "events": events.len(),
                    "mygraph": { "nodes": ours_created.0, "links": ours_created.1 },
                    "neo4j": { "nodes": theirs_created.0, "relationships": theirs_created.1 },
                    "errors": errors }),
        );
    }

    let mode = if o.mygraph_url.is_some() {
        "rest"
    } else {
        "in-process"
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "graph": o.graph, "mygraph": mode, "seed": o.seed, "runs": o.runs, "warmup": o.warmup,
            "floor": floor, "queries": report, "inserts": inserts,
        }))?
    );
    Ok(ok)
}
