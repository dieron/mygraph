//! The engine with the tantivy index attached: writes reach it, search uses it, and reopening
//! catches it up from the change log ([STORE-TEXT-1]).

use mygraph_core::engine::Graph;
use mygraph_store_sqlite::{GraphFile, Policies};
use mygraph_text_tantivy::TantivyText;
use serde_json::{Value as Json, json};
use std::path::Path;

const T: i64 = 1_761_300_000_000;

fn open(dir: &Path, attach: bool) -> Graph<GraphFile> {
    let mut g = Graph::open(GraphFile::open(dir, "g").unwrap(), "g", Policies::default()).unwrap();
    if attach {
        g.attach_text_index(Box::new(TantivyText::open(dir, "g").unwrap()))
            .unwrap();
    }
    g
}

fn write_person(g: &mut Graph<GraphFile>, name: &str, note: &str) {
    g.write(
        "agent",
        &json!({
            "evidence": [{ "ref": "@e", "kind": "text", "title": "Notes", "content": note }],
            "nodes": [{ "type": "Person", "name": name, "summary": note, "cite": ["@e"] }]
        }),
        T,
    )
    .unwrap();
}

fn text_hits(g: &Graph<GraphFile>, q: &str, section: &str) -> Vec<Json> {
    g.search(&json!({ "query": q, "mode": "text", "in": [section] }))
        .unwrap()["hits"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn writes_reach_the_index_and_reopen_catches_up() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    // Data written before the index exists is picked up by the first attach (rebuild).
    let mut g = open(dir.path(), false);
    write_person(
        &mut g,
        "Ada Lovelace",
        "wrote the first published algorithm",
    );
    drop(g);
    let mut g = open(dir.path(), true);
    assert_eq!(text_hits(&g, "algorithm", "nodes")[0]["id"], "n_1");
    // Writes go to the index right away (read-your-writes).
    write_person(&mut g, "Grace Hopper", "built the first compiler");
    assert_eq!(text_hits(&g, "compiler", "nodes")[0]["id"], "n_2");
    let ev = text_hits(&g, "compil", "evidence");
    assert_eq!(ev[0]["id"], "e_2");
    assert_eq!(ev[0]["snippet"], "Notes built the first «compiler»");
    drop(g);
    // Writes made without the index are caught up from the change log on the next attach.
    let mut g = open(dir.path(), false);
    write_person(&mut g, "Alan Turing", "described the universal machine");
    drop(g);
    let g = open(dir.path(), true);
    assert_eq!(text_hits(&g, "universal", "nodes")[0]["id"], "n_3");
    assert_eq!(text_hits(&g, "compiler", "nodes")[0]["id"], "n_2");
}
