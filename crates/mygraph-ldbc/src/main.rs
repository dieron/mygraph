//! `mygraph-ldbc`: loads an LDBC SNB Interactive v1 dataset (`CsvMergeForeign`,
//! `StringDateFormatter`) into a MyGraph graph through the engine's own `write`, straight from the
//! `.tar.zst` archive, and reports counts, time and throughput (docs/11-benchmarks.md §2).
//!
//! Three passes over the archive: the person list tables (emails, languages), then every node
//! table, then every link (foreign-key columns of the node tables and the edge tables), so every
//! endpoint exists before its links are written.

mod archive;
mod neo4j;
mod schema;

use archive::{Result, col, each_table, rows};
use clap::{Parser, Subcommand, ValueEnum};
use mygraph_core::engine::Graph;
use mygraph_core::model::{Policies, RequireEvidence};
use mygraph_store_sqlite::{GraphFile, graph_path};
use schema::{NODE_TABLES, PERSON_LISTS, PersonLists};
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ACTOR: &str = "ldbc-loader";
/// Batch timestamps start here and advance by one second per batch, so files are reproducible.
const T0: i64 = 1_761_700_000_000;
/// [OP-WRITE-1]
const MAX_ITEMS: usize = 1000;
const DATASETS: &str = "https://datasets.ldbcouncil.org/snb-interactive-v1";

#[derive(Parser)]
#[command(name = "mygraph-ldbc", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Loads an LDBC SNB archive into a new graph.
    Load {
        /// The `.tar.zst` archive as downloaded from LDBC.
        #[arg(long)]
        archive: PathBuf,
        /// Data directory (graph files go to <data>/graphs/).
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "ldbc")]
        graph: String,
        #[arg(long, value_enum, default_value_t = Provenance::File)]
        provenance: Provenance,
        /// Delete an existing graph file of the same name first.
        #[arg(long)]
        replace: bool,
    },
    /// Writes the same graph as CSV files for `neo4j-admin database import full`, plus an
    /// argument file `import.args`.
    Neo4jCsv {
        #[arg(long)]
        archive: PathBuf,
        /// Output folder.
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Provenance {
    /// No evidence; the graph's policy `require_evidence` is `off`.
    None,
    /// One evidence per CSV file; every node and link cites the file it came from.
    File,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Load {
            archive,
            data,
            graph,
            provenance,
            replace,
        } => load(&archive, &data, &graph, provenance, replace),
        Cmd::Neo4jCsv { archive, out } => neo4j::export(&archive, &out),
    }
}

#[derive(Default)]
struct Tally {
    rows: u64,
    created: u64,
    matched: u64,
    /// Rows whose endpoint is not in the graph.
    dangling: u64,
}

struct Pending {
    items: Vec<Json>,
    /// Per item: the node table index and LDBC id (nodes) or the link type (links).
    tags: Vec<(usize, i64, &'static str)>,
    size: usize,
}

struct Loader {
    g: Graph<GraphFile>,
    t: i64,
    provenance: Provenance,
    archive_name: String,
    evidence: HashMap<String, String>,
    /// Per node table: LDBC id → node ID.
    ids: Vec<HashMap<i64, String>>,
    pending: Pending,
    /// Per node type and per link type.
    tally: BTreeMap<String, Tally>,
    batches: u64,
    progress: Instant,
    start: Instant,
}

impl Loader {
    fn cite(&self, ev: &Option<String>) -> Option<Json> {
        ev.as_ref().map(|e| json!([e]))
    }

    /// The evidence for one file of the archive, created on first use.
    fn evidence_for(&mut self, rel: &str) -> Result<Option<String>> {
        if self.provenance == Provenance::None {
            return Ok(None);
        }
        if let Some(id) = self.evidence.get(rel) {
            return Ok(Some(id.clone()));
        }
        let item = json!({
            "key": format!("ldbc-snb/{}/{rel}", self.archive_name),
            "kind": "dataset",
            "uri": format!("{DATASETS}/{}#{rel}", self.archive_name),
            "title": format!("LDBC SNB {rel}"),
            "publisher": "LDBC",
        });
        let res = self.write(json!({ "evidence": [item] }))?;
        let id = res["items"][0]["id"]
            .as_str()
            .ok_or("no evidence id")?
            .to_owned();
        self.evidence.insert(rel.to_owned(), id.clone());
        Ok(Some(id))
    }

    fn write(&mut self, req: Json) -> Result<Json> {
        self.t += 1000;
        self.batches += 1;
        self.g
            .write(ACTOR, &req, self.t)
            .map_err(|e| e.to_json().to_string().into())
    }

    fn push(&mut self, item: Json, tag: (usize, i64, &'static str), section: &str) -> Result<()> {
        let size = 1 + usize::from(item.get("cite").is_some());
        if self.pending.size + size > MAX_ITEMS {
            self.flush(section)?;
        }
        self.pending.items.push(item);
        self.pending.tags.push(tag);
        self.pending.size += size;
        Ok(())
    }

    /// Writes the pending items of one section ("nodes" or "links") as one batch.
    fn flush(&mut self, section: &str) -> Result<()> {
        if self.pending.items.is_empty() {
            return Ok(());
        }
        let items = std::mem::take(&mut self.pending.items);
        let tags = std::mem::take(&mut self.pending.tags);
        self.pending.size = 0;
        let res = self.write(json!({ section: items }))?;
        let outcomes = res["items"]
            .as_array()
            .ok_or("no items in write result")?
            .iter()
            .filter(|i| i["section"] == section);
        for (out, &(table, ldbc_id, link)) in outcomes.zip(&tags) {
            let key = if section == "nodes" {
                NODE_TABLES[table].ty
            } else {
                link
            };
            let tally = self.tally.entry(key.to_owned()).or_default();
            if out["outcome"] == "created" {
                tally.created += 1;
            } else {
                tally.matched += 1;
            }
            if section == "nodes" {
                let id = out["id"].as_str().ok_or("no node id")?.to_owned();
                self.ids[table].insert(ldbc_id, id);
            }
        }
        if self.progress.elapsed() >= Duration::from_secs(10) {
            let (n, l) = (self.g.state().nodes.len(), self.g.state().links.len());
            let s = self.start.elapsed().as_secs_f64();
            eprintln!("[{s:>7.1}s] {n} nodes, {l} links, {} batches", self.batches);
            self.progress = Instant::now();
        }
        Ok(())
    }

    fn node_rows(
        &mut self,
        rel: &str,
        table: usize,
        r: &mut dyn BufRead,
        lists: &PersonLists,
    ) -> Result<()> {
        let spec = &NODE_TABLES[table];
        let ev = self.evidence_for(rel)?;
        let (header, rows) = rows(r)?;
        let cols = spec.columns(&header)?;
        for row in rows {
            let n = spec.row(&cols, &row?, lists)?;
            let mut item = json!({
                "key": n.key,
                "type": spec.ty,
                "name": n.name,
                "props": n.props,
            });
            if !n.labels.is_empty() {
                item["labels"] = json!(n.labels);
            }
            if let Some(c) = self.cite(&ev) {
                item["cite"] = c;
            }
            self.tally.entry(spec.ty.to_owned()).or_default().rows += 1;
            self.push(item, (table, n.id, ""), "nodes")?;
        }
        self.flush("nodes")
    }

    fn link(
        &mut self,
        ty: &'static str,
        ends: [(usize, &str); 2],
        directed: bool,
        props: Map<String, Json>,
        ev: &Option<String>,
    ) -> Result<()> {
        let tally = self.tally.entry(ty.to_owned()).or_default();
        tally.rows += 1;
        let id = |(t, s): (usize, &str), ids: &Vec<HashMap<i64, String>>| {
            s.parse::<i64>().ok().and_then(|i| ids[t].get(&i).cloned())
        };
        let (Some(from), Some(to)) = (id(ends[0], &self.ids), id(ends[1], &self.ids)) else {
            tally.dangling += 1;
            return Ok(());
        };
        let mut item = json!({ "type": ty, "from": from, "to": to, "directed": directed });
        if !props.is_empty() {
            item["props"] = Json::Object(props);
        }
        if let Some(c) = self.cite(ev) {
            item["cite"] = c;
        }
        self.push(item, (0, 0, ty), "links")
    }

    /// Links from the foreign-key columns of a node table.
    fn fk_rows(&mut self, rel: &str, table: usize, r: &mut dyn BufRead) -> Result<()> {
        let spec = &NODE_TABLES[table];
        let ev = self.evidence_for(rel)?;
        let (header, rows) = rows(r)?;
        let id_col = col(&header, "id")?;
        let fks = spec
            .fks
            .iter()
            .map(|f| {
                let target = schema::node_table(f.target).ok_or("unknown target")?.0;
                Ok((col(&header, f.col)?, target, f))
            })
            .collect::<Result<Vec<_>>>()?;
        for row in rows {
            let row = row?;
            for &(c, target, f) in &fks {
                if row[c].is_empty() {
                    continue;
                }
                let mut ends = [(table, row[id_col].as_str()), (target, row[c].as_str())];
                if f.reverse {
                    ends.swap(0, 1);
                }
                self.link(f.link, ends, true, Map::new(), &ev)?;
            }
        }
        self.flush("links")
    }

    fn edge_rows(
        &mut self,
        rel: &str,
        spec: &schema::EdgeTable,
        r: &mut dyn BufRead,
    ) -> Result<()> {
        let ev = self.evidence_for(rel)?;
        let (header, rows) = rows(r)?;
        let from = schema::node_table(spec.from).ok_or("unknown table")?.0;
        let to = schema::node_table(spec.to).ok_or("unknown table")?.0;
        for row in rows {
            let row = row?;
            let mut props = Map::new();
            for (i, h) in header.iter().enumerate().skip(2) {
                if let Some(v) = schema::value(h, &row[i]) {
                    props.insert(h.clone(), v);
                }
            }
            let ends = [(from, row[0].as_str()), (to, row[1].as_str())];
            self.link(spec.link, ends, spec.directed, props, &ev)?;
        }
        self.flush("links")
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn file_size(path: &Path) -> u64 {
    ["", "-wal"]
        .iter()
        .filter_map(|s| std::fs::metadata(format!("{}{s}", path.display())).ok())
        .map(|m| m.len())
        .sum()
}

fn load(
    archive: &Path,
    data: &Path,
    graph: &str,
    provenance: Provenance,
    replace: bool,
) -> Result<()> {
    let path = graph_path(data, graph);
    if replace {
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
    }
    let archive_name = archive
        .file_name()
        .ok_or("archive has no file name")?
        .to_string_lossy()
        .into_owned();
    let sha256 = sha256_of(archive)?;
    eprintln!("{archive_name} (SHA-256 {sha256}) -> {}", path.display());
    let policies = Policies {
        require_evidence: match provenance {
            Provenance::None => RequireEvidence::Off,
            Provenance::File => RequireEvidence::Enforce,
        },
        ..Policies::default()
    };
    GraphFile::create(data, graph, &policies, T0)?;
    let g = Graph::open(GraphFile::open(data, graph)?, graph, policies)?;
    let mut l = Loader {
        g,
        t: T0,
        provenance,
        archive_name,
        evidence: HashMap::new(),
        ids: vec![HashMap::new(); NODE_TABLES.len()],
        pending: Pending {
            items: Vec::new(),
            tags: Vec::new(),
            size: 0,
        },
        tally: BTreeMap::new(),
        batches: 0,
        progress: Instant::now(),
        start: Instant::now(),
    };

    // Pass 1: the tables that become list properties of persons.
    let lists = archive::person_lists(archive)?;

    // Pass 2: nodes.
    let mut unknown = Vec::new();
    each_table(archive, |rel, table, r| {
        if let Some((t, _)) = schema::node_table(table) {
            l.node_rows(rel, t, r, &lists)
        } else {
            if schema::edge_table(table).is_none() && !PERSON_LISTS.iter().any(|p| p.0 == table) {
                unknown.push(rel.to_owned());
            }
            Ok(())
        }
    })?;
    let t_nodes = l.start.elapsed();
    drop(lists);

    // Pass 3: links.
    each_table(archive, |rel, table, r| {
        if let Some((t, _)) = schema::node_table(table) {
            l.fk_rows(rel, t, r)
        } else if let Some(spec) = schema::edge_table(table) {
            l.edge_rows(rel, spec, r)
        } else {
            Ok(())
        }
    })?;
    let t_all = l.start.elapsed();

    let st = l.g.state();
    let tally: Map<String, Json> = l
        .tally
        .iter()
        .map(|(k, t)| {
            (
                k.clone(),
                json!({ "rows": t.rows, "created": t.created, "matched": t.matched, "dangling": t.dangling }),
            )
        })
        .collect();
    let (n, links) = (st.nodes.len(), st.links.len());
    let secs = |d: Duration| (d.as_secs_f64() * 10.0).round() / 10.0;
    let mut report = json!({
        "archive": l.archive_name,
        "archive_sha256": sha256,
        "graph": graph,
        "provenance": match provenance { Provenance::None => "none", Provenance::File => "file" },
        "counts": {
            "nodes": n, "links": links,
            "evidence": st.evidence.len(), "citations": st.citations.len(),
        },
        "by_type": tally,
        "unknown_tables": unknown,
        "batches": l.batches,
        "seconds": { "nodes": secs(t_nodes), "links": secs(t_all - t_nodes), "total": secs(t_all) },
        "per_second": {
            "nodes": (n as f64 / t_nodes.as_secs_f64()).round(),
            "links": (links as f64 / (t_all - t_nodes).as_secs_f64()).round(),
        },
    });
    drop(l);
    report["file_size_mib"] = json!((file_size(&path) as f64 / 1048576.0 * 10.0).round() / 10.0);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
