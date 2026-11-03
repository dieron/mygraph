//! The engine with the semantic index attached ([RANK-8], [RANK-9], [STORE-VEC-1]): writes reach
//! it, search and resolve use it, and reopening catches it up from the change log.
//!
//! Most tests use a fake embedding model, so they need no model files. The tests named
//! `model_*` use the real model from `MYGRAPH_MODEL_DIR` and are skipped (with a note) when it
//! is not set.

use mygraph_core::engine::Graph;
use mygraph_core::store::{Closeness, DerivedIndex, StoreFailure};
use mygraph_semantic::{Embed, Model, Role, UsearchIndex, index_dir};
use mygraph_store_sqlite::{GraphFile, Policies};
use serde_json::{Value as Json, json};
use std::path::Path;
use std::sync::Arc;

const T: i64 = 1_761_300_000_000;

/// Words with a meaning in common share a dimension; other words get one each (by hash).
struct Fake(&'static str);

fn concept(word: &str) -> &str {
    match word {
        "smoker" | "blender" | "appliance" | "fryer" => "appliance",
        "plant" | "plants" | "fern" | "orchid" => "plant",
        "ibm" | "international" | "business" | "machines" => "ibm",
        w => w,
    }
}

impl Embed for Fake {
    fn id(&self) -> String {
        self.0.to_owned()
    }
    fn dims(&self) -> usize {
        256
    }
    fn embed(&self, texts: &[String], _: Role) -> Result<Vec<Vec<f32>>, StoreFailure> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; 256];
                for w in t
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                {
                    let c = concept(&w.to_lowercase()).to_owned();
                    let h = c
                        .bytes()
                        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
                    v[(h % 256) as usize] += 1.0;
                }
                mygraph_semantic::model::normalize(&mut v);
                v
            })
            .collect())
    }
    fn floor(&self, closeness: Closeness) -> f32 {
        match closeness {
            Closeness::Related => 0.2,
            Closeness::Close => 0.6,
        }
    }
}

fn open(dir: &Path, model: Option<Arc<dyn Embed>>) -> Graph<GraphFile> {
    let mut g = Graph::open(GraphFile::open(dir, "g").unwrap(), "g", Policies::default()).unwrap();
    if let Some(m) = model {
        g.attach_semantic_index(Box::new(UsearchIndex::open(dir, "g", m).unwrap()))
            .unwrap();
    }
    g
}

fn fake() -> Option<Arc<dyn Embed>> {
    Some(Arc::new(Fake("fake-1")))
}

fn say(g: &mut Graph<GraphFile>, messages: &[&str]) {
    let evidence: Vec<Json> = messages
        .iter()
        .map(|m| json!({ "kind": "text", "title": "Chat", "content": m }))
        .collect();
    g.write("agent", &json!({ "evidence": evidence }), T)
        .unwrap();
}

fn hits(g: &Graph<GraphFile>, q: &str, mode: &str, section: &str) -> Vec<Json> {
    g.search(&json!({ "query": q, "mode": mode, "in": [section] }))
        .unwrap()["hits"]
        .as_array()
        .unwrap()
        .clone()
}

fn ids(hits: &[Json]) -> Vec<&str> {
    hits.iter().map(|h| h["id"].as_str().unwrap()).collect()
}

#[test]
fn search_finds_by_meaning_and_fuses_with_words() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), fake());
    say(
        &mut g,
        &[
            "I just got a smoker today",
            "We hiked up the hill",
            "My new appliance arrived, a blender",
        ],
    );
    assert_eq!(g.semantic_index_status(), "current");
    // Read-your-writes: a query embeds what waits.
    let found = hits(&g, "kitchen appliance", "semantic", "evidence");
    assert_eq!(ids(&found), ["e_3", "e_1"]);
    assert!(found.iter().all(|h| h["matched"] == "semantic"));
    // The start of the closest passage of the content (the title is not embedded).
    assert_eq!(found[1]["snippet"], "I just got a smoker today");
    // Words alone miss the smoker; auto adds it after the word match.
    assert_eq!(ids(&hits(&g, "appliance", "text", "evidence")), ["e_3"]);
    let auto = hits(&g, "appliance", "auto", "evidence");
    assert_eq!(ids(&auto), ["e_3", "e_1"]);
    assert_eq!(auto[0]["matched"], "text");
    assert_eq!(auto[1]["matched"], "semantic");
    // Unrelated in meaning and in words: nothing.
    assert!(hits(&g, "orchid", "auto", "evidence").is_empty());
}

#[test]
fn nodes_are_found_by_meaning_below_name_matches() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), fake());
    g.write(
        "agent",
        &json!({
            "evidence": [{ "ref": "@e", "kind": "text", "content": "notes" }],
            "nodes": [
                { "type": "Thing", "name": "Fern", "cite": ["@e"] },
                { "type": "Thing", "name": "Plant pot", "cite": ["@e"] },
            ]
        }),
        T,
    )
    .unwrap();
    let found = hits(&g, "plant", "auto", "nodes");
    assert_eq!(ids(&found), ["n_2", "n_1"]);
    assert_eq!(found[0]["matched"], "name:prefix");
    assert_eq!(found[1]["matched"], "semantic");
}

#[test]
fn resolve_lists_similar_nodes_without_changing_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), fake());
    g.write(
        "agent",
        &json!({
            "evidence": [{ "ref": "@e", "kind": "text", "content": "notes" }],
            "nodes": [{ "type": "Organization", "name": "International Business Machines", "cite": ["@e"] }]
        }),
        T,
    )
    .unwrap();
    let r = g
        .resolve(&json!({ "candidates": [{ "name": "IBM" }, { "name": "Hiking club" }] }))
        .unwrap();
    assert_eq!(r["results"][0]["verdict"], "none");
    assert_eq!(r["results"][0]["similar"][0]["id"], "n_1");
    assert_eq!(
        r["results"][0]["similar"][0]["name"],
        "International Business Machines"
    );
    assert_eq!(r["results"][1], json!({ "verdict": "none" }));
}

#[test]
fn reopening_catches_up_and_another_model_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    // Data written before the index exists: the first attach rebuilds.
    let mut g = open(dir.path(), None);
    say(&mut g, &["I just got a smoker today"]);
    drop(g);
    let mut g = open(dir.path(), fake());
    assert_eq!(ids(&hits(&g, "appliance", "semantic", "evidence")), ["e_1"]);
    say(&mut g, &["A fern for the window"]);
    g.commit_semantic().unwrap();
    let meta: Json = serde_json::from_str(
        &std::fs::read_to_string(index_dir(dir.path(), "g").join("meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["applied"], g.last_seq());
    assert_eq!(meta["model"], "fake-1");
    drop(g);
    // Writes made without the index are caught up from the change log on the next attach.
    let mut g = open(dir.path(), None);
    say(&mut g, &["The blender broke"]);
    drop(g);
    let index = UsearchIndex::open(dir.path(), "g", fake().unwrap()).unwrap();
    assert!(index.applied_seq().is_some());
    drop(index);
    let g = open(dir.path(), fake());
    assert_eq!(
        ids(&hits(&g, "appliance", "semantic", "evidence")),
        ["e_3", "e_1"]
    );
    assert_eq!(ids(&hits(&g, "plant", "semantic", "evidence")), ["e_2"]);
    drop(g);
    // An index built by another model is not used.
    let other: Arc<dyn Embed> = Arc::new(Fake("fake-2"));
    let index = UsearchIndex::open(dir.path(), "g", other.clone()).unwrap();
    assert_eq!(index.applied_seq(), None);
    drop(index);
    let g = open(dir.path(), Some(other));
    assert_eq!(ids(&hits(&g, "plant", "semantic", "evidence")), ["e_2"]);
}

#[test]
fn mode_semantic_needs_an_index() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), None);
    say(&mut g, &["I just got a smoker today"]);
    assert_eq!(g.semantic_index_status(), "none");
    let e = g
        .search(&json!({ "query": "appliance", "mode": "semantic", "in": ["evidence"] }))
        .unwrap_err();
    assert_eq!(e.reason.code(), "search.semantic_unavailable");
    // Auto matches words only.
    assert_eq!(ids(&hits(&g, "smoker", "auto", "evidence")), ["e_1"]);
    assert!(hits(&g, "appliance", "auto", "evidence").is_empty());
}

#[test]
fn many_waiting_documents_make_the_index_build_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), fake());
    let messages: Vec<String> = (0..100)
        .map(|i| format!("message {i} about a smoker"))
        .collect();
    let refs: Vec<&str> = messages.iter().map(String::as_str).collect();
    say(&mut g, &refs);
    // Too many to embed in a query: words only, until commits have embedded them.
    assert_eq!(g.semantic_index_status(), "building");
    assert_eq!(hits(&g, "appliance", "auto", "evidence").len(), 0);
    g.commit_semantic().unwrap();
    assert_eq!(g.semantic_index_status(), "current");
    assert_eq!(hits(&g, "appliance", "auto", "evidence").len(), 20);
}

// ---- with the real model -------------------------------------------------------------------

fn model() -> Option<Arc<dyn Embed>> {
    match std::env::var_os("MYGRAPH_MODEL_DIR") {
        Some(dir) => Some(Arc::new(Model::load(Path::new(&dir)).unwrap())),
        None => {
            eprintln!("skipped: set MYGRAPH_MODEL_DIR to the embedding model's folder");
            None
        }
    }
}

/// Messages from LongMemEval histories (docs/11-benchmarks.md §4, finding 39): the answers to
/// "what kitchen appliance did I buy" and "how many plants did I acquire", among others.
const MESSAGES: &[&str] = &[
    "I'm looking for some recommendations on different types of wood for smoking meats. I just got a smoker and I'm excited to experiment with different types of wood and meats today.",
    "I'm also wondering if I should repot my snake plant, which I got from my sister last month. Do you think it would benefit from a bigger pot and some fresh soil?",
    "Can you suggest some upbeat songs for my morning workout playlist? I like pop and rock.",
    "I've been thinking about planning a trip to Japan next spring. What are the best cities to visit?",
    "My manager asked me to lead the new project at work, and I'm a bit nervous about it.",
    "What's a good way to keep track of my monthly budget? I keep overspending on dining out.",
    "I finally finished reading that mystery novel my book club picked, the ending was a surprise.",
    "Smoker: A smoker helps calm the bees by producing a gentle stream of smoke.",
    "How long should I let my sourdough starter rest before baking?",
    "I've been trying to run three times a week, but my knees hurt after long runs.",
];

#[test]
fn model_finds_messages_phrased_differently() {
    let Some(m) = model() else { return };
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), Some(m));
    say(&mut g, MESSAGES);
    for (query, want) in [("kitchen appliance", "e_1"), ("acquire plants", "e_2")] {
        // No message has the words; by meaning the right one is among the first.
        assert!(hits(&g, query, "text", "evidence").is_empty(), "{query}");
        let found = hits(&g, query, "auto", "evidence");
        eprintln!(
            "{query}: {:?}",
            found
                .iter()
                .map(|h| (&h["id"], &h["score"]))
                .collect::<Vec<_>>()
        );
        let at = found.iter().position(|h| h["id"] == want);
        assert!(at.is_some_and(|i| i < 3), "{query}: {found:?}");
        assert_eq!(found[at.unwrap()]["matched"], "semantic");
    }
}

#[test]
fn model_lists_a_name_spelled_out_as_similar() {
    let Some(m) = model() else { return };
    let dir = tempfile::tempdir().unwrap();
    GraphFile::create(dir.path(), "g", &Policies::default(), T).unwrap();
    let mut g = open(dir.path(), Some(m));
    g.write(
        "agent",
        &json!({
            "evidence": [{ "ref": "@e", "kind": "text", "content": "notes" }],
            "nodes": [
                { "type": "Organization", "name": "International Business Machines", "cite": ["@e"] },
                { "type": "Person", "name": "Ada Lovelace", "cite": ["@e"] },
                { "type": "City", "name": "Paris", "cite": ["@e"] },
            ]
        }),
        T,
    )
    .unwrap();
    let r = g
        .resolve(&json!({ "candidates": [{ "name": "IBM" }, { "name": "Mary Jones" }] }))
        .unwrap();
    eprintln!("{r}");
    assert_eq!(r["results"][0]["verdict"], "none");
    assert_eq!(r["results"][0]["similar"][0]["id"], "n_1");
    // Another person's name is not close.
    assert_eq!(r["results"][1], json!({ "verdict": "none" }));
}
