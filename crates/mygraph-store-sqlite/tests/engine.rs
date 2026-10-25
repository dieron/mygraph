//! The engine on a real SQLite graph file: write, matching, policy, retract/restore, merge,
//! change log and reopen.

use mygraph_core::engine::Graph;
use mygraph_core::model::Status;
use mygraph_store_sqlite::{GraphFile, Policies, RequireEvidence};
use serde_json::{Value as Json, json};
use std::path::Path;

const T: i64 = 1_761_300_000_000;
const ACTOR: &str = "test-agent";

fn open(dir: &Path) -> Graph<GraphFile> {
    let file = GraphFile::open(dir, "g").unwrap();
    let policies = file.policies().unwrap();
    Graph::open(file, "g", policies).unwrap()
}

fn new_graph(dir: &Path, policies: Policies) -> Graph<GraphFile> {
    GraphFile::create(dir, "g", &policies, T).unwrap();
    open(dir)
}

fn ada_batch() -> Json {
    json!({
        "evidence": [
            { "ref": "@art", "kind": "url", "uri": "https://example.org/ada", "title": "Ada", "source_date": {"$date": "2024-03-02"} },
            { "ref": "@frag", "kind": "text", "parent": "@art", "content": "In 1843 Lovelace published her notes on Babbage's Analytical Engine." }
        ],
        "nodes": [
            { "ref": "@ada", "type": "person", "name": "Ada Lovelace", "aliases": ["Augusta Ada King"],
              "props": { "born": {"$date": "1815-12-10"} }, "cite": ["@frag"] },
            { "ref": "@bab", "type": "Person", "name": "Charles Babbage", "cite": ["@frag"] },
            { "ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@frag"] }
        ],
        "links": [
            { "type": "collaborated with", "from": "@ada", "to": "@bab", "directed": false,
              "cite": [{ "evidence": "@frag", "confidence": 0.9 }] },
            { "type": "WROTE_ABOUT", "from": "@ada", "to": "@ae", "props": { "year": 1843 }, "cite": ["@frag"] }
        ]
    })
}

fn count(g: &Graph<GraphFile>, sql: &str) -> i64 {
    g.store()
        .connection()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

#[test]
fn write_creates_everything_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    let r = g.write(ACTOR, &ada_batch(), T).unwrap();
    assert_eq!(r["batch"], "b_1");
    assert_eq!(
        r["counts"]["created"],
        json!({ "evidence": 2, "node": 3, "link": 2, "citation": 5 })
    );
    assert_eq!(r["refs"]["@ada"], "n_1");
    // Vocabulary was canonicalized and announced.
    assert_eq!(&*g.state().nodes[&1].type_, "Person");
    assert_eq!(&*g.state().links[&1].type_, "COLLABORATED_WITH");
    assert!(
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "NEW_VOCABULARY")
    );

    // The same write again changes nothing and creates no batch.
    let r2 = g.write(ACTOR, &ada_batch(), T + 1).unwrap();
    assert_eq!(r2["batch"], Json::Null);
    assert_eq!(r2["counts"]["created"], json!({}));
    assert_eq!(count(&g, "SELECT count(*) FROM node"), 3);
    assert_eq!(count(&g, "SELECT count(*) FROM batch"), 1);
    assert_eq!(count(&g, "SELECT count(*) FROM changelog"), 12);
}

#[test]
fn state_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let before: Vec<_> = {
        let mut v: Vec<_> = g.state().nodes.values().cloned().collect();
        v.sort_by_key(|n| n.idx);
        v
    };
    drop(g);
    let mut g = open(dir.path());
    let mut after: Vec<_> = g.state().nodes.values().cloned().collect();
    after.sort_by_key(|n| n.idx);
    assert_eq!(before, after);
    assert_eq!(g.state().links.len(), 2);
    assert_eq!(g.state().citations.len(), 5);
    // Counters continue after reopen.
    let r = g
        .write(
            ACTOR,
            &json!({ "evidence": [{ "kind": "observation", "content": "x" }] }),
            T,
        )
        .unwrap();
    assert_eq!(r["items"][0]["id"], "e_3");
    // Evidence content is on disk, not in memory.
    assert_eq!(g.state().evidence[&2].content, None);
    assert!(
        g.store()
            .connection()
            .query_row("SELECT content FROM evidence WHERE idx = 2", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap()
            .starts_with("In 1843")
    );
}

#[test]
fn name_matching_merges_aliases_and_detects_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(
        dir.path(),
        Policies {
            require_evidence: RequireEvidence::Off,
            ..Policies::default()
        },
    );
    g.write(ACTOR, &ada_batch(), T).unwrap();
    // Different spelling of an alias matches by name key; the new spelling becomes an alias.
    let r = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "Person", "name": "augusta ada king" }] }),
            T,
        )
        .unwrap();
    assert_eq!(r["items"][0]["outcome"], "matched");
    let r = g.write(ACTOR, &json!({ "nodes": [{ "type": "Person", "name": "ADA LOVELACE", "summary": "Mathematician" }] }), T).unwrap();
    assert_eq!(r["items"][0]["outcome"], "updated");
    assert_eq!(
        g.state().nodes[&1].summary.as_deref(),
        Some("Mathematician")
    );
    assert_eq!(
        g.state().nodes[&1].aliases,
        vec!["Augusta Ada King".to_string()]
    );

    g.write(ACTOR, &json!({ "nodes": [{ "type": "Person", "name": "John Smith" }, { "type": "Person", "name": "John Smith", "create_new": true }] }), T).unwrap();
    let e = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "Person", "name": "john smith" }] }),
            T,
        )
        .unwrap_err();
    assert_eq!(e.reason.code(), "write.ambiguous_match");
    assert_eq!(e.details["candidates"].as_array().unwrap().len(), 2);
    assert_eq!((e.section.as_deref(), e.index), (Some("nodes"), Some(0)));
}

#[test]
fn property_conflicts_follow_on_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let w = |born: &str, mode: &str| {
        json!({
            "nodes": [{ "id": "n_1", "props": { "born": {"$date": born} } }],
            "options": { "on_conflict": mode }
        })
    };
    let r = g.write(ACTOR, &w("1816", "keep"), T).unwrap();
    assert_eq!(r["items"][0]["outcome"], "matched");
    assert_eq!(r["warnings"][0]["code"], "PROPERTY_CONFLICT");
    let e = g.write(ACTOR, &w("1816", "error"), T).unwrap_err();
    assert_eq!(e.reason.code(), "write.property_conflict");
    let r = g.write(ACTOR, &w("1816", "overwrite"), T).unwrap();
    assert_eq!(r["items"][0]["outcome"], "updated");
    assert_eq!(
        g.node_props_json(1).unwrap()["born"],
        json!({"$date": "1816"})
    );
    assert_eq!(g.state().nodes[&1].meta.version, 2);
}

#[test]
fn evidence_policy_is_enforced_and_failed_batches_leave_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    let e = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "Person", "name": "Nobody" }] }),
            T,
        )
        .unwrap_err();
    assert_eq!(e.reason.code(), "write.evidence_required");
    assert_eq!(e.details["unsupported"], json!(["n_1"]));
    assert!(g.state().nodes.is_empty());
    assert!(g.state().actors.is_empty());
    // A contradicting citation alone is not support.
    let e = g.write(ACTOR, &json!({
        "evidence": [{ "ref": "@o", "kind": "observation", "content": "doubt" }],
        "nodes": [{ "type": "Person", "name": "Nobody", "cite": [{ "evidence": "@o", "role": "contradicts" }] }]
    }), T).unwrap_err();
    assert_eq!(e.reason.code(), "write.evidence_required");
    // Numbers of failed batches are not consumed.
    let r = g
        .write(
            ACTOR,
            &json!({
                "evidence": [{ "ref": "@o", "kind": "observation", "content": "I met them" }],
                "nodes": [{ "type": "Person", "name": "Somebody", "cite": ["@o"] }]
            }),
            T,
        )
        .unwrap();
    assert_eq!(r["refs"]["@o"], "e_1");
    assert_eq!(r["refs"]["n_1"], Json::Null);
    assert_eq!(r["items"][1]["id"], "n_1");
}

#[test]
fn dry_run_reports_but_does_not_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    let mut req = ada_batch();
    req["options"] = json!({ "dry_run": true });
    let r = g.write(ACTOR, &req, T).unwrap();
    assert_eq!(r["batch"], Json::Null);
    assert_eq!(r["counts"]["created"]["node"], 3);
    assert_eq!(r["refs"]["@ada"], Json::Null);
    assert!(g.state().nodes.is_empty());
    assert_eq!(count(&g, "SELECT count(*) FROM node"), 0);
    // A real write afterwards starts numbering at 1.
    assert_eq!(
        g.write(ACTOR, &ada_batch(), T).unwrap()["refs"]["@ada"],
        "n_1"
    );
}

#[test]
fn undirected_links_match_in_either_order_and_evidence_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let r = g.write(ACTOR, &json!({
        "evidence": [{ "kind": "text", "parent": "e_1", "content": "In 1843 Lovelace published her notes on Babbage's Analytical Engine." }],
        "links": [{ "type": "COLLABORATED_WITH", "from": "n_2", "to": "n_1", "directed": false, "props": { "since": 1833 } }]
    }), T).unwrap();
    assert_eq!(r["items"][0]["outcome"], "matched");
    assert_eq!(r["items"][0]["id"], "e_2");
    assert_eq!(r["items"][1]["outcome"], "updated");
    assert_eq!(r["items"][1]["id"], "l_1");
}

#[test]
fn retract_cascades_and_restore_brings_back_exactly_the_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    // Retract one link directly, then the node: only the other link is in the node's cascade.
    g.retract(ACTOR, &json!({ "targets": ["l_2"], "reason": "unsure" }), T)
        .unwrap();
    let r = g.retract(ACTOR, &json!({ "targets": ["n_1"] }), T).unwrap();
    assert_eq!(
        r["items"],
        json!([
            { "id": "n_1", "outcome": "retracted" },
            { "id": "l_1", "outcome": "retracted", "cascade_of": "n_1" }
        ])
    );
    assert!(!g.state().names.contains_key("ada lovelace"));
    let e = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "id": "n_1", "summary": "x" }] }),
            T,
        )
        .unwrap_err();
    assert_eq!(e.reason.code(), "lifecycle.retracted");

    let r = g
        .retract(
            ACTOR,
            &json!({ "action": "restore", "targets": ["n_1"] }),
            T,
        )
        .unwrap();
    assert_eq!(
        r["items"],
        json!([
            { "id": "n_1", "outcome": "restored" },
            { "id": "l_1", "outcome": "restored", "cascade_of": "n_1" }
        ])
    );
    assert_eq!(g.state().links[&2].status, Status::Retracted);
    assert_eq!(g.state().nodes[&1].meta.version, 3);

    // Retracting evidence warns about facts that lose their only support.
    let r = g.retract(ACTOR, &json!({ "targets": ["e_2"] }), T).unwrap();
    let unsupported: Vec<_> = r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "UNSUPPORTED_FACT")
        .collect();
    assert_eq!(unsupported.len(), 4);
}

#[test]
fn retract_by_batch_and_change_log_records_causes() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let r = g
        .retract(
            ACTOR,
            &json!({ "batch": "b_1", "reason": "wrong article" }),
            T,
        )
        .unwrap();
    assert_eq!(r["batch"], "b_2");
    assert_eq!(
        g.state()
            .nodes
            .values()
            .filter(|n| n.status == Status::Active)
            .count(),
        0
    );
    // Cascaded entries point at the entry that caused them.
    let n: i64 = count(
        &g,
        "SELECT count(*) FROM changelog c JOIN changelog p ON json_extract(c.diff, '$.cascade_of') = p.seq WHERE c.op = 'retract'",
    );
    assert!(n > 0);
    let r = g
        .retract(ACTOR, &json!({ "action": "restore", "batch": "b_2" }), T)
        .unwrap();
    assert!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["outcome"] != "unchanged" || i.get("cascade_of").is_some())
    );
    assert_eq!(
        g.state()
            .nodes
            .values()
            .filter(|n| n.status == Status::Active)
            .count(),
        3
    );
}

#[test]
fn merge_moves_links_and_citations_and_old_ids_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    g.write(ACTOR, &json!({
        "evidence": [{ "ref": "@o", "kind": "observation", "content": "duplicate" }],
        "nodes": [{ "ref": "@dup", "type": "Person", "name": "C. Babbage", "props": { "born": 1791 }, "cite": ["@o"] }],
        "links": [{ "type": "COLLABORATED_WITH", "from": "@dup", "to": "n_1", "directed": false, "cite": ["@o"] }]
    }), T).unwrap();
    let r = g
        .merge(ACTOR, &json!({ "into": "n_2", "merge": ["n_4"] }), T)
        .unwrap();
    assert_eq!(r["combined"]["links"], 1);
    let sv = &g.state().nodes[&2];
    assert!(sv.aliases.contains(&"C. Babbage".to_string()));
    assert_eq!(g.node_props_json(2).unwrap()["born"], 1791);
    assert_eq!(g.state().nodes[&4].status, Status::Merged);
    // The old ID and name now resolve to the survivor.
    let r = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "id": "n_4", "summary": "Mathematician" }] }),
            T,
        )
        .unwrap();
    assert_eq!(r["items"][0]["id"], "n_2");
    let r = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "Person", "name": "C. BABBAGE" }] }),
            T,
        )
        .unwrap();
    assert_eq!(r["items"][0]["id"], "n_2");
    assert_eq!(
        g.merge(ACTOR, &json!({ "into": "n_2", "merge": ["n_2"] }), T)
            .unwrap_err()
            .reason
            .code(),
        "merge.self"
    );
}

#[test]
fn reference_and_validation_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    let code =
        |g: &mut Graph<GraphFile>, req: Json| g.write(ACTOR, &req, T).unwrap_err().reason.code();
    assert_eq!(
        code(
            &mut g,
            json!({ "links": [{ "type": "X", "from": "@a", "to": "@b" }] })
        ),
        "ref.undefined"
    );
    assert_eq!(
        code(
            &mut g,
            json!({ "links": [{ "type": "X", "from": "n_9", "to": "n_8" }] })
        ),
        "ref.not_found"
    );
    assert_eq!(
        code(&mut g, json!({ "evidence": [{ "kind": "text" }] })),
        "evidence.empty"
    );
    assert_eq!(
        code(
            &mut g,
            json!({ "evidence": [{ "kind": "url", "uri": "not a uri" }] })
        ),
        "evidence.uri_invalid"
    );
    assert_eq!(
        code(&mut g, json!({ "nodes": [{ "type": "3D", "name": "x" }] })),
        "vocab.bad_start"
    );
    assert_eq!(
        code(
            &mut g,
            json!({ "nodes": [{ "type": "Person", "name": "'" }] })
        ),
        "name.empty_key"
    );
    assert_eq!(
        code(
            &mut g,
            json!({ "evidence": [
        { "ref": "@a", "kind": "observation", "content": "a" },
        { "ref": "@a", "kind": "observation", "content": "b" }] })
        ),
        "ref.duplicate"
    );
}

#[test]
fn blobs_are_uploaded_matched_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    // "hello" in base64; its SHA-256 is well known.
    let hash = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let blob_file = dir.path().join("blobs/sha256/2c/f2").join(hash);
    let upload = json!({ "evidence": [{ "kind": "file", "blob_base64": "aGVsbG8=", "mime": "text/plain" }] });

    // [OP-WRITE-9] a dry run stores nothing.
    let mut dry = upload.clone();
    dry["options"] = json!({ "dry_run": true });
    g.write(ACTOR, &dry, T).unwrap();
    assert!(!blob_file.exists());

    let r = g.write(ACTOR, &upload, T).unwrap();
    assert_eq!(r["items"][0]["outcome"], "created");
    assert_eq!(std::fs::read(&blob_file).unwrap(), b"hello");
    let e = &g.state().evidence[&1];
    assert_eq!(e.blob_sha256.as_deref(), Some(hash));
    assert_eq!(e.blob_size, Some(5));

    // [OP-WRITE-8] referring to the stored blob matches the same evidence.
    let r = g
        .write(
            ACTOR,
            &json!({ "evidence": [{ "kind": "file", "blob": { "sha256": hash } }] }),
            T,
        )
        .unwrap();
    assert_eq!(r["items"][0]["outcome"], "matched");

    let err = |g: &mut Graph<GraphFile>, item: Json| {
        g.write(ACTOR, &json!({ "evidence": [item] }), T)
            .unwrap_err()
            .reason
            .code()
    };
    assert_eq!(
        err(
            &mut g,
            json!({ "kind": "file", "blob": { "sha256": "0".repeat(64) } })
        ),
        "evidence.blob_missing"
    );
    assert_eq!(
        err(
            &mut g,
            json!({ "kind": "file", "blob": { "sha256": "ABC" } })
        ),
        "write.invalid_field"
    );
    assert_eq!(
        err(&mut g, json!({ "kind": "file", "blob_base64": "aGVsbG8" })),
        "evidence.blob_invalid"
    );
    assert_eq!(
        err(
            &mut g,
            json!({ "kind": "file", "blob_base64": "aGVs bG8=" })
        ),
        "evidence.blob_invalid"
    );
    assert_eq!(
        err(
            &mut g,
            json!({ "kind": "file", "blob_base64": "aGVsbG8=", "blob": { "sha256": hash } })
        ),
        "evidence.blob_conflict"
    );
    assert_eq!(
        err(&mut g, json!({ "id": "e_1", "blob_base64": "d29ybGQ=" })),
        "evidence.immutable_field"
    );
    // The blob survives reopening, and the record says so.
    drop(g);
    let g = open(dir.path());
    assert_eq!(g.state().evidence[&1].blob_size, Some(5));
}

#[test]
fn new_vocabulary_lists_similar_items() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let r = g
        .write(
            ACTOR,
            &json!({
                "evidence": [{ "ref": "@o", "kind": "observation", "content": "seen" }],
                "nodes": [{ "ref": "@p", "type": "Persons", "name": "Grace Hopper", "props": { "born_on": {"$date": "1906"} }, "cite": ["@o"] }],
                "links": [{ "type": "WROTE_ABOUTS", "from": "@p", "to": { "type": "Machine", "name": "Analytical Engine" }, "cite": ["@o"] }]
            }),
            T + 1,
        )
        .unwrap();
    let similar = |kind: &str| -> Json {
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "NEW_VOCABULARY" && w["details"]["kind"] == kind)
            .map(|w| w["details"]["similar"].clone())
            .unwrap()
    };
    // [VSIM-3] counts are usage counts: two Person nodes, one WROTE_ABOUT link.
    assert_eq!(
        similar("node_type"),
        json!([{ "name": "Person", "count": 2 }])
    );
    assert_eq!(
        similar("link_type"),
        json!([{ "name": "WROTE_ABOUT", "count": 1 }])
    );
    assert_eq!(similar("prop_key"), json!([{ "name": "born", "count": 1 }]));
}

#[test]
fn search_ranks_names_then_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let ids = |r: &Json| -> Vec<(String, String)> {
        r["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["id"].as_str().unwrap().to_owned(),
                    h["matched"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let s = |g: &Graph<GraphFile>, q: Json| g.search(&q).unwrap();
    let pair = |id: &str, m: &str| (id.to_owned(), m.to_owned());

    // [RANK-4] exact name, alias, prefix and fuzzy.
    let r = s(&g, json!({ "query": "ADA LOVELACE" }));
    assert_eq!(ids(&r)[0], pair("n_1", "name:exact"));
    assert_eq!(r["hits"][0]["score"], json!(1.0));
    let r = s(&g, json!({ "query": "augusta ada king" }));
    assert_eq!(ids(&r)[0], pair("n_1", "alias:exact"));
    let r = s(&g, json!({ "query": "charles bab" }));
    assert_eq!(ids(&r)[0], pair("n_2", "name:prefix"));
    let r = s(&g, json!({ "query": "Charles Babage" }));
    assert_eq!(ids(&r)[0], pair("n_2", "name:fuzzy"));
    // One word of a name: trigram similarity 8/16, above full text.
    let r = s(&g, json!({ "query": "babbage" }));
    assert_eq!(ids(&r), vec![pair("n_2", "name:fuzzy")]);
    let r = s(&g, json!({ "query": "babbage", "mode": "text" }));
    assert_eq!(ids(&r), vec![pair("n_2", "text")]);
    // Type filter and modes.
    let r = s(
        &g,
        json!({ "query": "analytical engine", "types": ["machine"] }),
    );
    assert_eq!(ids(&r), vec![pair("n_3", "name:exact")]);
    let r = s(
        &g,
        json!({ "query": "analytical engine", "types": ["Person"] }),
    );
    assert_eq!(r["hits"], json!([]));
    let r = s(&g, json!({ "query": "babbage", "mode": "exact" }));
    assert_eq!(r["hits"], json!([]));
    // Evidence by full text, with a snippet; links by type words.
    let r = s(&g, json!({ "query": "analytical", "in": ["evidence"] }));
    assert_eq!(ids(&r), vec![pair("e_2", "text")]);
    assert!(
        r["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("«Analytical»")
    );
    let r = s(&g, json!({ "query": "wrote", "in": ["links"] }));
    assert_eq!(ids(&r), vec![pair("l_2", "text")]);
    // Limit and truncation.
    let r = s(&g, json!({ "query": "a", "limit": 1 }));
    assert_eq!(r["truncated"], json!(true));
    // Retracted nodes leave name search.
    g.retract(ACTOR, &json!({ "targets": ["n_2"] }), T).unwrap();
    let r = s(&g, json!({ "query": "charles babbage" }));
    assert_eq!(r["hits"], json!([]));
    let r = s(&g, json!({ "query": "babbage", "include_retracted": true }));
    assert_eq!(r["hits"][0]["status"], "retracted");
    let e = g.search(&json!({ "query": "" })).unwrap_err();
    assert_eq!(e.reason.code(), "search.query_invalid");
}

#[test]
fn get_returns_objects_with_neighbourhood_support_and_content() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    let r = g
        .get(&json!({ "refs": ["n_1", { "type": "Person", "name": "charles babbage" }, "nope", "e_2"],
                      "include": { "content": { "from": 3, "max_chars": 4 } } }))
        .unwrap();
    let ada = &r["items"][0];
    assert_eq!(ada["name"], "Ada Lovelace");
    assert!(
        ada.get("version").is_none(),
        "normal detail has no record-time fields"
    );
    // [OP-GET-4] COLLABORATED_WITH is undirected, WROTE_ABOUT directed.
    assert_eq!(
        ada["links"]["counts"],
        json!([{ "type": "COLLABORATED_WITH", "direction": "both", "count": 1 },
               { "type": "WROTE_ABOUT", "direction": "out", "count": 1 }])
    );
    assert_eq!(
        ada["links"]["items"][1]["other"],
        json!({ "id": "n_3", "type": "Machine", "name": "Analytical Engine" })
    );
    // [OP-GET-5] one supporting citation from e_2.
    assert_eq!(ada["support"], json!({ "supports": 1, "sources": 1 }));
    assert_eq!(ada["evidence"]["items"][0]["id"], "e_2");
    assert_eq!(r["items"][1]["id"], "n_2");
    assert_eq!(r["items"][2]["error"]["code"], "ref.not_found");
    // [OP-GET-7] content range in code points.
    assert_eq!(r["items"][3]["content"], "1843");
    assert_eq!(r["items"][3]["content_truncated"], true);

    let r = g
        .get(&json!({ "refs": ["l_1"], "detail": "brief" }))
        .unwrap();
    assert_eq!(
        r["items"][0],
        json!({ "id": "l_1", "type": "COLLABORATED_WITH", "from": "n_1", "to": "n_2", "directed": false, "status": "active" })
    );
    let r = g.get(&json!({ "refs": ["n_1"], "detail": "full", "include": { "links": { "limit": 1, "types": ["wrote about"] } } })).unwrap();
    assert_eq!(r["items"][0]["version"], 1);
    assert_eq!(
        r["items"][0]["links"]["counts"].as_array().unwrap().len(),
        1
    );
    // A merged node's ID resolves to the survivor.
    g.write(
        ACTOR,
        &json!({ "evidence": [{ "ref": "@o", "kind": "observation", "content": "dup" }],
        "nodes": [{ "type": "Person", "name": "A. Lovelace", "cite": ["@o"] }] }),
        T,
    )
    .unwrap();
    g.merge(ACTOR, &json!({ "into": "n_1", "merge": ["n_4"] }), T)
        .unwrap();
    let r = g
        .get(&json!({ "refs": ["n_4"], "detail": "brief" }))
        .unwrap();
    assert_eq!(r["items"][0]["id"], "n_1");
    assert_eq!(r["items"][0]["merged_from"], "n_4");
    assert_eq!(
        g.get(&json!({ "refs": [] })).unwrap_err().reason.code(),
        "get.invalid_field"
    );
}
