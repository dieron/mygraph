//! `mygraph-bench`: builds deterministic synthetic graphs through the engine's own `write`
//! operation, and measures how long a graph takes to load and how much memory it uses.
//!
//! Dataset shape (generator version 1):
//! - nodes of 6 types with ~5 properties each, 15 % with an alias, 30 % with a summary;
//! - one evidence item per node (text of 2–6 sentences, ~300 bytes), cited by that node;
//! - links between endpoints drawn with a skewed law (a few hubs, a long tail), typed by the
//!   endpoint types, each citing its source node's evidence;
//! - batches of at most 1000 items, as an agent would send them ([OP-WRITE-1]).

mod data;
mod rng;

use clap::{Parser, Subcommand};
use mygraph_core::engine::Graph;
use mygraph_core::state::State;
use mygraph_core::store::Store;
use mygraph_store_sqlite::{GraphFile, Policies, graph_path};
use rng::Rng;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Counts live heap bytes, so memory per object can be measured exactly.
#[global_allocator]
static ALLOC: cap::Cap<std::alloc::System> = cap::Cap::new(std::alloc::System, usize::MAX);

const GENERATOR_VERSION: u32 = 1;
const ACTOR: &str = "bench-generator";
/// Batch timestamps start here and advance by one second per batch, so files are reproducible.
const T0: i64 = 1_761_300_000_000;

#[derive(Parser)]
#[command(name = "mygraph-bench", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generates a synthetic graph.
    Generate {
        /// Data directory (graph files go to <data>/graphs/).
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "bench")]
        graph: String,
        #[arg(long, default_value_t = 10_000)]
        nodes: usize,
        /// Links to create; default 5 per node.
        #[arg(long)]
        links: Option<usize>,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Delete an existing graph file of the same name first.
        #[arg(long)]
        replace: bool,
    },
    /// Opens a graph and reports load time, memory and index sizes.
    Load {
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "bench")]
        graph: String,
        /// Also report heap bytes per object, split into rows and indexes.
        #[arg(long)]
        breakdown: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match Cli::parse().cmd {
        Cmd::Generate {
            data,
            graph,
            nodes,
            links,
            seed,
            replace,
        } => generate(
            &data,
            &graph,
            nodes,
            links.unwrap_or(nodes * 5),
            seed,
            replace,
        ),
        Cmd::Load {
            data,
            graph,
            breakdown,
        } => load(&data, &graph, breakdown),
    }
}

struct Progress {
    start: Instant,
    last: Instant,
}

impl Progress {
    fn tick(&mut self, what: &str, done: usize, total: usize) {
        if self.last.elapsed() >= Duration::from_secs(10) || done == total {
            let s = self.start.elapsed().as_secs_f64();
            eprintln!(
                "[{s:>7.1}s] {what}: {done}/{total} ({:.0}/s)",
                done as f64 / s.max(1e-9)
            );
            self.last = Instant::now();
        }
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn ids_of(res: &Json, section: &str) -> Vec<String> {
    res["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["section"] == section)
        .map(|i| i["id"].as_str().expect("id").to_owned())
        .collect()
}

fn generate(
    data: &Path,
    graph: &str,
    n_nodes: usize,
    n_links: usize,
    seed: u64,
    replace: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = graph_path(data, graph);
    if replace {
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
    }
    eprintln!(
        "generator v{GENERATOR_VERSION}, seed {seed}: {n_nodes} nodes, {n_links} links -> {}",
        path.display()
    );
    let mut t = T0;
    GraphFile::create(data, graph, &Policies::default(), t)?;
    let mut g = Graph::open(GraphFile::open(data, graph)?, graph, Policies::default())?;
    let mut r = Rng::new(seed);
    let mut pools = data::Pools::new(&mut r);
    let mut p = Progress {
        start: Instant::now(),
        last: Instant::now(),
    };

    // Phase 1: nodes, each with its own evidence (3 items per node).
    let mut node_ids = Vec::with_capacity(n_nodes);
    let mut node_types = Vec::with_capacity(n_nodes);
    let mut node_evidence = Vec::with_capacity(n_nodes);
    while node_ids.len() < n_nodes {
        let n = (n_nodes - node_ids.len()).min(333);
        let (mut nodes, mut evidence) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for j in 0..n {
            let ty = pools.node_type(&mut r);
            let (node, ev) = pools.node_with_evidence(&mut r, ty, &format!("@e{j}"));
            nodes.push(node);
            evidence.push(ev);
            node_types.push(ty);
        }
        t += 1000;
        let res = g
            .write(ACTOR, &json!({ "evidence": evidence, "nodes": nodes }), t)
            .map_err(|e| e.to_json().to_string())?;
        let created = ids_of(&res, "nodes");
        assert_eq!(created.len(), n, "every node item creates a node");
        node_ids.extend(created);
        node_evidence.extend(ids_of(&res, "evidence"));
        p.tick("nodes", node_ids.len(), n_nodes);
    }

    // Phase 2: links (2 items per link). Endpoints are skewed, then scattered over the node
    // indexes with a multiplier coprime to the node count, so hubs are of every type.
    let mut mult = 1_000_003 % n_nodes.max(1);
    while n_nodes > 1 && gcd(mult.max(1), n_nodes) != 1 {
        mult += 1;
    }
    let mult = mult.max(1);
    let endpoint = |r: &mut Rng| (r.skewed(n_nodes) * mult) % n_nodes;
    let mut made = 0usize;
    let mut matched = 0usize;
    while made < n_links && n_nodes > 1 {
        let n = (n_links - made).min(500);
        let mut links = Vec::with_capacity(n);
        while links.len() < n {
            let (mut a, mut b) = (endpoint(&mut r), endpoint(&mut r));
            if a == b {
                continue;
            }
            let (ty, directed, swap) = data::link_type(node_types[a], node_types[b]);
            if swap {
                std::mem::swap(&mut a, &mut b);
            }
            let mut link = json!({
                "type": ty, "from": node_ids[a], "to": node_ids[b], "directed": directed,
                "cite": [node_evidence[a]],
            });
            data::link_extras(&mut r, &mut link);
            links.push(link);
        }
        t += 1000;
        let res = g
            .write(ACTOR, &json!({ "links": links }), t)
            .map_err(|e| e.to_json().to_string())?;
        let c = res["counts"]["created"]["link"].as_u64().unwrap_or(0) as usize;
        made += c;
        matched += n - c;
        p.tick("links", made, n_links);
    }
    let st = g.state();
    eprintln!(
        "done in {:.1}s: {} nodes, {} links ({matched} link items matched existing links), {} evidence, {} citations",
        p.start.elapsed().as_secs_f64(),
        st.nodes.len(),
        st.links.len(),
        st.evidence.len(),
        st.citations.len()
    );
    drop(g);
    eprintln!("file size: {}", mib(file_size(&path)));
    Ok(())
}

fn file_size(path: &Path) -> u64 {
    ["", "-wal"]
        .iter()
        .filter_map(|s| std::fs::metadata(format!("{}{s}", path.display())).ok())
        .map(|m| m.len())
        .sum()
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn rss() -> u64 {
    memory_stats::memory_stats().map_or(0, |m| m.physical_mem as u64)
}

fn heap() -> usize {
    ALLOC.allocated()
}

fn per(bytes: isize, n: usize) -> Json {
    json!(if n == 0 { 0 } else { bytes / n as isize })
}

fn load(
    data: &Path,
    graph: &str,
    breakdown: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (rss0, heap0) = (rss(), heap());
    let t = Instant::now();
    let file = GraphFile::open(data, graph)?;
    // As `Graph::open` does: rows stream from SQLite straight into the indexes.
    let state = State::load(&file, &file.counters()?)?;
    let t_load = t.elapsed();
    let (rss1, heap1) = (rss(), heap());
    let size = file_size(&graph_path(data, graph));
    let mut report = json!({
        "graph": graph,
        "file_size_mib": size as f64 / 1048576.0,
        "counts": {
            "nodes": state.nodes.len(), "links": state.links.len(),
            "evidence": state.evidence.len(), "citations": state.citations.len(),
            "name_keys": state.names.len(), "adjacency_nodes": state.adjacency.len(),
        },
        "load_seconds": t_load.as_secs_f64(),
        "memory_mib": {
            "heap_graph_state": heap1.saturating_sub(heap0) as f64 / 1048576.0,
            "process_rss": rss1 as f64 / 1048576.0,
            "process_rss_growth": rss1.saturating_sub(rss0) as f64 / 1048576.0,
        },
    });
    drop(state);
    if breakdown {
        report["bytes_per_object"] = breakdown_of(&file)?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Heap bytes per object: the rows themselves, and what the indexes add on top of them.
fn breakdown_of(file: &GraphFile) -> Result<Json, Box<dyn std::error::Error + Send + Sync>> {
    // Rows: free each category of a snapshot and see how much heap it held.
    let mut s = file.load()?;
    let mut rows = Vec::new();
    type Take = fn(&mut mygraph_core::store::Snapshot);
    let takes: [Take; 4] = [
        |s| drop(std::mem::take(&mut s.nodes)),
        |s| drop(std::mem::take(&mut s.links)),
        |s| drop(std::mem::take(&mut s.evidence)),
        |s| drop(std::mem::take(&mut s.citations)),
    ];
    for take in takes {
        let before = heap();
        take(&mut s);
        rows.push((before - heap()) as isize);
    }
    // State: grow one category at a time, the way `State::load` does (slots reserved first).
    // Moving a row keeps its heap parts; the clone's vector buffer is freed when consumed.
    use mygraph_core::model::{Citation, Evidence, Link, Node};
    let s = file.load()?;
    let c = file.counters()?;
    let counts = [
        s.nodes.len(),
        s.links.len(),
        s.evidence.len(),
        s.citations.len(),
    ];
    let inline = [
        size_of::<Node>(),
        size_of::<Link>(),
        size_of::<Evidence>(),
        size_of::<Citation>(),
    ];
    let slack = [
        (s.nodes.capacity() - s.nodes.len()) * inline[0],
        (s.links.capacity() - s.links.len()) * inline[1],
        (s.evidence.capacity() - s.evidence.len()) * inline[2],
        (s.citations.capacity() - s.citations.len()) * inline[3],
    ];
    let mut st = State::default();
    let mut grow = Vec::new();
    let mut step = |f: &mut dyn FnMut(&mut State)| {
        let before = heap() as isize;
        f(&mut st);
        grow.push(heap() as isize - before);
    };
    let (nodes, links, evidence, citations) = (s.nodes, s.links, s.evidence, s.citations);
    step(&mut |st| {
        st.nodes.reserve_to(c.node);
        nodes.clone().into_iter().for_each(|n| drop(st.put_node(n)))
    });
    step(&mut |st| {
        st.links.reserve_to(c.link);
        links.clone().into_iter().for_each(|l| drop(st.put_link(l)))
    });
    step(&mut |st| {
        st.evidence.reserve_to(c.evidence);
        evidence
            .clone()
            .into_iter()
            .for_each(|e| drop(st.put_evidence(e)))
    });
    step(&mut |st| {
        st.citations.reserve_to(c.citation);
        citations
            .clone()
            .into_iter()
            .for_each(|c| drop(st.put_citation(c)))
    });
    let mut out = serde_json::Map::new();
    for (i, name) in ["node", "link", "evidence", "citation"].iter().enumerate() {
        // As read from the store: the struct plus the heap it owns (strings, maps).
        let heap_parts = rows[i] - (slack[i] + inline[i] * counts[i]) as isize;
        out.insert(
            (*name).to_owned(),
            json!({
                "struct": inline[i],
                "owned_heap_as_read": per(heap_parts, counts[i]),
                "in_state_total": per(grow[i], counts[i]),
            }),
        );
    }
    Ok(Json::Object(out))
}
