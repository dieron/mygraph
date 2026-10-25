//! Randomized operation sequences and a kill-during-write test.
//!
//! After every operation: the invariants of spec/model/objects.md hold, the database passes
//! SQLite's integrity and foreign-key checks, and the in-memory state equals a fresh load from
//! disk (so memory never holds anything the disk does not, and vice versa).

use mygraph_core::engine::Graph;
use mygraph_core::model::Status;
use mygraph_core::state::State;
use mygraph_core::store::Store;
use mygraph_store_sqlite::{GraphFile, Policies};
use proptest::prelude::*;
use serde_json::json;
use std::path::Path;

const ACTOR: &str = "prop-agent";
const T: i64 = 1_761_300_000_000;

#[derive(Debug, Clone)]
enum Op {
    /// Create or match a node (small name space, so names collide often).
    Node {
        name: u8,
        type_: u8,
    },
    Link {
        a: u8,
        b: u8,
        type_: u8,
        directed: bool,
    },
    Retract {
        kind: u8,
        pick: u8,
    },
    Restore {
        kind: u8,
        pick: u8,
    },
    RetractEvidence {
        pick: u8,
    },
    Merge {
        into: u8,
        other: u8,
    },
    SetProp {
        pick: u8,
        value: i32,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u8..12, 0u8..2).prop_map(|(name, type_)| Op::Node { name, type_ }),
        3 => (any::<u8>(), any::<u8>(), 0u8..2, any::<bool>()).prop_map(|(a, b, type_, directed)| Op::Link { a, b, type_, directed }),
        1 => (0u8..2, any::<u8>()).prop_map(|(kind, pick)| Op::Retract { kind, pick }),
        1 => (0u8..2, any::<u8>()).prop_map(|(kind, pick)| Op::Restore { kind, pick }),
        1 => any::<u8>().prop_map(|pick| Op::RetractEvidence { pick }),
        1 => (any::<u8>(), any::<u8>()).prop_map(|(into, other)| Op::Merge { into, other }),
        1 => (any::<u8>(), any::<i32>()).prop_map(|(pick, value)| Op::SetProp { pick, value }),
    ]
}

fn pick(ids: &[i64], n: u8) -> Option<i64> {
    (!ids.is_empty()).then(|| ids[n as usize % ids.len()])
}

fn sorted_ids<T>(m: &mygraph_core::table::Table<T>, keep: impl Fn(&T) -> bool) -> Vec<i64> {
    m.iter().filter(|(_, x)| keep(x)).map(|(k, _)| k).collect()
}

fn run(g: &mut Graph<GraphFile>, op: &Op, step: i64) {
    let st = g.state();
    let active_nodes = sorted_ids(&st.nodes, |n| n.status == Status::Active);
    let types = ["Person", "Place"];
    let link_types = ["KNOWS", "NEAR"];
    let obs = |text: String| json!({ "ref": "@o", "kind": "observation", "content": text });
    let result = match op {
        Op::Node { name, type_ } => g.write(ACTOR, &json!({
            "evidence": [obs(format!("node {name}"))],
            "nodes": [{ "type": types[*type_ as usize], "name": format!("Name {name}"), "cite": ["@o"] }]
        }), T + step),
        Op::Link { a, b, type_, directed } => {
            let (Some(a), Some(b)) = (pick(&active_nodes, *a), pick(&active_nodes, *b)) else { return };
            g.write(ACTOR, &json!({
                "evidence": [obs(format!("link {a} {b}"))],
                "links": [{ "type": link_types[*type_ as usize], "from": format!("n_{}", enc(a)), "to": format!("n_{}", enc(b)),
                            "directed": directed, "cite": ["@o"] }]
            }), T + step)
        }
        Op::Retract { kind, pick: p } | Op::Restore { kind, pick: p } => {
            let restore = matches!(op, Op::Restore { .. });
            let want = if restore { Status::Retracted } else { Status::Active };
            let (prefix, ids) = if *kind == 0 {
                ("n", sorted_ids(&st.nodes, |n| n.status == want))
            } else {
                ("l", sorted_ids(&st.links, |l| l.status == want))
            };
            let Some(id) = pick(&ids, *p) else { return };
            g.retract(ACTOR, &json!({ "action": if restore { "restore" } else { "retract" }, "targets": [format!("{prefix}_{}", enc(id))] }), T + step)
        }
        Op::RetractEvidence { pick: p } => {
            let ids = sorted_ids(&st.evidence, |e| e.status == Status::Active);
            let Some(id) = pick(&ids, *p) else { return };
            g.retract(ACTOR, &json!({ "targets": [format!("e_{}", enc(id))] }), T + step)
        }
        Op::Merge { into, other } => {
            let (Some(a), Some(b)) = (pick(&active_nodes, *into), pick(&active_nodes, *other)) else { return };
            if a == b {
                return;
            }
            g.merge(ACTOR, &json!({ "into": format!("n_{}", enc(a)), "merge": [format!("n_{}", enc(b))] }), T + step)
        }
        Op::SetProp { pick: p, value } => {
            let Some(id) = pick(&active_nodes, *p) else { return };
            g.write(ACTOR, &json!({ "updates": [{ "target": { "node": format!("n_{}", enc(id)) }, "set": { "score": value } }] }), T + step)
        }
    };
    if let Err(e) = result {
        // Domain refusals are fine (e.g. restoring a link whose endpoint is retracted);
        // a storage failure means the engine produced something the schema rejects.
        assert_ne!(e.reason.code(), "storage.failure", "{op:?}: {e}");
    }
}

fn enc(n: i64) -> String {
    let id =
        mygraph_core::ids::ObjectId::new(mygraph_core::ids::ObjectKind::Node, n as u64).encode();
    id[2..].to_owned()
}

fn check(g: &Graph<GraphFile>, dir: &Path) {
    let st = g.state();
    // [OBJ invariants] active links connect active nodes; merged nodes point at existing nodes.
    for l in st.links.values().filter(|l| l.status == Status::Active) {
        assert_eq!(
            st.nodes[&l.src].status,
            Status::Active,
            "link {} has inactive src",
            l.idx
        );
        assert_eq!(
            st.nodes[&l.dst].status,
            Status::Active,
            "link {} has inactive dst",
            l.idx
        );
    }
    for n in st.nodes.values() {
        assert_eq!(n.status == Status::Merged, n.merged_into.is_some());
        if let Some(s) = n.merged_into {
            assert!(
                st.resolve_node(s)
                    .is_some_and(|r| st.nodes[&r].status != Status::Merged)
            );
        }
    }
    // Active citations point at existing evidence and targets.
    for c in st.citations.values() {
        assert!(st.evidence.contains_key(&c.evidence));
    }
    // SQLite checks.
    let conn = g.store().connection();
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let fk: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
    // Memory equals disk.
    let fresh = State::from_snapshot(GraphFile::open(dir, "g").unwrap().load().unwrap());
    assert_eq!(st.nodes, fresh.nodes);
    assert_eq!(st.links, fresh.links);
    assert_eq!(st.evidence, fresh.evidence);
    assert_eq!(st.citations, fresh.citations);
    assert_eq!(st.names, fresh.names);
    assert_eq!(st.adjacency, fresh.adjacency);
    assert_eq!(st.cites_by_evidence, fresh.cites_by_evidence);
    assert_eq!(st.active_evidence_kinds, fresh.active_evidence_kinds);
    assert_eq!(st.active_citations, fresh.active_citations);
    assert_eq!(
        st.active_citations,
        st.citations
            .values()
            .filter(|c| c.status == Status::Active)
            .count()
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn random_operation_sequences_keep_invariants(ops in prop::collection::vec(op(), 1..40)) {
        let dir = tempfile::tempdir().unwrap();
        GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
        let file = GraphFile::open(dir.path(), "g").unwrap();
        let mut g = Graph::open(file, "g", Policies::default()).unwrap();
        let mut last = g.store().counters().unwrap();
        for (i, op) in ops.iter().enumerate() {
            run(&mut g, op, i as i64);
            check(&g, dir.path());
            // [ID-LIFE-2] numbers only grow.
            let now = g.store().counters().unwrap();
            prop_assert!(now.node >= last.node && now.link >= last.link && now.seq >= last.seq);
            last = now;
        }
    }
}

/// Child half of the kill test: writes until killed. Runs only when the parent sets the env var.
#[test]
#[ignore = "run by kill_during_writes_leaves_a_consistent_file"]
fn kill_child_writer() {
    let Ok(dir) = std::env::var("MYGRAPH_KILL_TEST_DIR") else {
        return;
    };
    let file = GraphFile::open(Path::new(&dir), "g").unwrap();
    let mut g = Graph::open(file, "g", Policies::default()).unwrap();
    for i in 0..i64::MAX {
        let nodes: Vec<_> = (0..20)
            .map(|j| json!({ "type": "Person", "name": format!("P {i} {j}"), "cite": ["@o"] }))
            .collect();
        g.write(ACTOR, &json!({ "evidence": [{ "ref": "@o", "kind": "observation", "content": format!("batch {i}") }], "nodes": nodes }), T + i)
            .unwrap();
    }
}

#[test]
fn kill_during_writes_leaves_a_consistent_file() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["kill_child_writer", "--exact", "--ignored", "--nocapture"])
        .env("MYGRAPH_KILL_TEST_DIR", dir.path())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().unwrap();
    child.wait().unwrap();

    let file = GraphFile::open(dir.path(), "g").unwrap();
    let g = Graph::open(file, "g", Policies::default()).unwrap();
    let batches: i64 = g
        .store()
        .connection()
        .query_row("SELECT count(*) FROM batch", [], |r| r.get(0))
        .unwrap();
    assert!(batches > 0, "the child wrote nothing before it was killed");
    // Every committed batch is complete: 1 evidence + 20 nodes + 20 citations.
    assert_eq!(g.state().nodes.len() as i64, batches * 20);
    assert_eq!(g.state().citations.len() as i64, batches * 20);
    check(&g, dir.path());
}
