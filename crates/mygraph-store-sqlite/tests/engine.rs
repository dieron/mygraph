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
            { "ref": "@ada", "type": "Person", "name": "Ada Lovelace", "aliases": ["Augusta Ada King"],
              "props": { "born": {"$date": "1815-12-10"} }, "cite": ["@frag"] },
            { "ref": "@bab", "type": "Person", "name": "Charles Babbage", "cite": ["@frag"] },
            { "ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@frag"] }
        ],
        "links": [
            { "type": "COLLABORATED_WITH", "from": "@ada", "to": "@bab", "directed": false,
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
    // New vocabulary was announced.
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
        json!({ "query": "analytical engine", "types": ["Machine"] }),
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
    let r = g.get(&json!({ "refs": ["n_1"], "detail": "full", "include": { "links": { "limit": 1, "types": ["WROTE_ABOUT"] } } })).unwrap();
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

#[test]
fn resolve_scores_names_types_and_hints() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(dir.path(), Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    g.write(
        ACTOR,
        &json!({
            "evidence": [{ "ref": "@o", "kind": "observation", "content": "two Babbages" }],
            "nodes": [
                { "id": "n_2", "props": { "born": {"$date": "1791-12-26"} } },
                { "type": "Person", "name": "Benjamin Babbage", "props": { "born": {"$date": "1753"} }, "cite": ["@o"] }
            ]
        }),
        T,
    )
    .unwrap();
    let r = g
        .resolve(&json!({ "candidates": [
            { "name": "ada lovelace", "type": "Person" },
            { "name": "Augusta Ada King" },
            { "name": "C. Babbage", "type": "Person", "hints": { "born": {"$date": "1791"} } },
            { "name": "C. Babbage", "type": "Person" },
            { "name": "Analytical Engine", "type": "Person" },
            { "name": "Luigi Menabrea" }
        ] }))
        .unwrap();
    let res = &r["results"];
    assert_eq!(res[0]["verdict"], "match");
    assert_eq!(res[0]["best"]["why"], json!(["name exact"]));
    assert_eq!(res[1]["best"]["why"], json!(["alias exact"]));
    // Trigram similarity 9/18 -> 0.5 + 0.2 = 0.7; the date hint adds 0.1 -> match.
    assert_eq!(res[2]["verdict"], "match");
    assert_eq!(res[2]["best"]["id"], "n_2");
    assert_eq!(res[2]["best"]["score"], json!(0.8));
    assert_eq!(
        res[2]["best"]["why"],
        json!(["name fuzzy 0.50", "born matches"])
    );
    // Without the hint 0.7 is plausible but not a match.
    assert_eq!(res[3]["verdict"], "ambiguous");
    // Right name, wrong type: 1.0 x 0.7.
    assert_eq!(res[4]["verdict"], "ambiguous");
    assert_eq!(
        res[4]["best"]["why"],
        json!(["name exact", "type differs (Machine)"])
    );
    assert_eq!(res[5], json!({ "verdict": "none" }));
    let e = g
        .resolve(&json!({ "candidates": [{ "type": "Person" }] }))
        .unwrap_err();
    assert_eq!(e.reason.code(), "resolve.invalid_field");
}

/// Ada (n_1) -COLLABORATED_WITH- Babbage (n_2); Ada -WROTE_ABOUT-> Engine (n_3);
/// Babbage -DESIGNED-> Engine; Menabrea (n_4) -WROTE_ABOUT-> Engine, valid from 1842.
fn traversal_graph(dir: &Path) -> Graph<GraphFile> {
    let mut g = new_graph(dir, Policies::default());
    g.write(ACTOR, &ada_batch(), T).unwrap();
    g.write(
        ACTOR,
        &json!({
            "evidence": [{ "ref": "@o", "kind": "observation", "content": "more" }],
            "nodes": [{ "ref": "@m", "type": "Person", "name": "Luigi Menabrea", "cite": ["@o"] }],
            "links": [
                { "type": "DESIGNED", "from": "n_2", "to": "n_3", "cite": ["@o"] },
                { "type": "WROTE_ABOUT", "from": "@m", "to": "n_3", "valid_from": {"$date": "1842"}, "cite": ["@o"] }
            ]
        }),
        T,
    )
    .unwrap();
    g
}

fn ids_of(v: &Json) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn neighbors_expands_by_hop_with_limits() {
    let dir = tempfile::tempdir().unwrap();
    let g = traversal_graph(dir.path());
    let r = g.neighbors(&json!({ "start": ["n_1"] })).unwrap();
    assert_eq!(ids_of(&r["nodes"]), ["n_1", "n_2", "n_3"]);
    assert_eq!(ids_of(&r["links"]), ["l_1", "l_2"]);
    assert_eq!(r["nodes"][2]["hop"], 1);
    // [OP-NEIGH-2] hop 2 outwards: Babbage's DESIGNED link joins two known nodes.
    let r = g
        .neighbors(&json!({ "start": ["n_1"], "depth": 2, "direction": "out" }))
        .unwrap();
    assert_eq!(ids_of(&r["links"]), ["l_1", "l_2", "l_3"]);
    assert_eq!(r["truncated"], false);
    // Steps with types; the node limit; the per-node limit with a note.
    let r = g
        .neighbors(&json!({ "start": ["n_3"], "steps": [{ "link_types": ["WROTE_ABOUT"], "direction": "in" }], "include": { "support": true } }))
        .unwrap();
    assert_eq!(ids_of(&r["nodes"]), ["n_3", "n_1", "n_4"]);
    assert_eq!(r["links"][0]["sources"], 1);
    let r = g
        .neighbors(&json!({ "start": ["n_1"], "limit": 2 }))
        .unwrap();
    assert_eq!(ids_of(&r["nodes"]), ["n_1", "n_2"]);
    assert_eq!(r["truncated"], true);
    let r = g
        .neighbors(&json!({ "start": ["n_1"], "limit_per_node": 1 }))
        .unwrap();
    assert_eq!(
        r["notes"],
        json!([{ "node": "n_1", "hop": 1, "links_taken": 1, "links_total": 2 }])
    );
    // [TRAV-2] as_of before 1842 hides Menabrea's link.
    let r = g
        .neighbors(&json!({ "start": ["n_4"], "as_of": {"$date": "1800"} }))
        .unwrap();
    assert_eq!(ids_of(&r["nodes"]), ["n_4"]);
    let e = g
        .neighbors(&json!({ "start": ["n_1"], "depth": 1, "steps": [{}] }))
        .unwrap_err();
    assert_eq!(e.reason.code(), "neighbors.invalid_field");
}

#[test]
fn paths_are_shortest_first_in_path_order() {
    let dir = tempfile::tempdir().unwrap();
    let g = traversal_graph(dir.path());
    let lens = |r: &Json| -> Vec<u64> {
        r["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["length"].as_u64().unwrap())
            .collect()
    };
    let r = g.paths(&json!({ "from": "n_1", "to": "n_3" })).unwrap();
    assert_eq!(lens(&r), [1]);
    assert_eq!(ids_of(&r["paths"][0]["links"]), ["l_2"]);
    assert_eq!(r["paths"][0]["min_sources"], 1);
    let r = g
        .paths(&json!({ "from": "n_1", "to": "n_3", "mode": "k_shortest", "k": 5 }))
        .unwrap();
    assert_eq!(lens(&r), [1, 2]);
    assert_eq!(ids_of(&r["paths"][1]["nodes"]), ["n_1", "n_2", "n_3"]);
    assert_eq!(r["truncated"], false);
    // Direction is read from `from` to `to`: nothing leaves the Engine outwards.
    let r = g
        .paths(&json!({ "from": "n_3", "to": "n_1", "direction": "out" }))
        .unwrap();
    assert_eq!(r["paths"], json!([]));
    // Menabrea reaches Ada through the Engine, but not as of 1800, nor within one step.
    let r = g.paths(&json!({ "from": "n_4", "to": "n_1" })).unwrap();
    assert_eq!(ids_of(&r["paths"][0]["nodes"]), ["n_4", "n_3", "n_1"]);
    let r = g
        .paths(&json!({ "from": "n_4", "to": "n_1", "as_of": {"$date": "1800"} }))
        .unwrap();
    assert_eq!(r["paths"], json!([]));
    let r = g
        .paths(&json!({ "from": "n_4", "to": "n_1", "max_length": 1 }))
        .unwrap();
    assert_eq!(r["paths"], json!([]));
    let r = g.paths(&json!({ "from": "n_2", "to": "n_2" })).unwrap();
    assert_eq!(lens(&r), [0]);
    // all_shortest: Ada to Babbage has one direct path.
    let r = g
        .paths(&json!({ "from": "n_1", "to": "n_2", "mode": "all_shortest" }))
        .unwrap();
    assert_eq!(lens(&r), [1]);
}

#[test]
fn history_by_object_and_since_seq() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    g.write(
        ACTOR,
        &json!({ "updates": [{ "target": { "node": "n_1" }, "summary": "mathematician" }] }),
        T + 5,
    )
    .unwrap();
    let r = g.history(&json!({ "target": "n_1" })).unwrap();
    let e = r["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2, "created, then updated");
    assert_eq!(e[0]["op"], "update", "newest first");
    assert_eq!(e[0]["diff"]["after"]["summary"], "mathematician");
    assert_eq!(e[0]["actor"], ACTOR);
    assert_eq!(e[1]["op"], "create");
    assert_eq!(r["more"], false);
    // Paging by before_seq.
    let first = g.history(&json!({ "target": "n_1", "limit": 1 })).unwrap();
    assert_eq!(first["more"], true);
    let seq = first["entries"][0]["seq"].as_i64().unwrap();
    let rest = g
        .history(&json!({ "target": "n_1", "before_seq": seq }))
        .unwrap();
    assert_eq!(rest["entries"][0]["op"], "create");
    // since_seq: oldest first, resumable with last_seq.
    let all = g.history(&json!({ "since_seq": 0, "limit": 500 })).unwrap();
    let last = all["last_seq"].as_i64().unwrap();
    assert_eq!(
        all["entries"].as_array().unwrap().last().unwrap()["seq"],
        last
    );
    let none = g.history(&json!({ "since_seq": last })).unwrap();
    assert_eq!(none["entries"], json!([]));
    let e = g
        .history(&json!({ "target": "n_1", "since_seq": 0 }))
        .unwrap_err();
    assert_eq!(e.reason.code(), "history.invalid_field");
}

#[test]
fn find_filters_groups_and_aggregates() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    g.write(
        ACTOR,
        &json!({ "evidence": [{ "ref": "@o", "kind": "observation", "content": "dates" }],
                 "updates": [
                     { "target": { "node": "n_2" }, "set": { "born": {"$date": "1791-12-26"}, "country": "Britain" }, "cite": ["@o"] },
                     { "target": { "node": "n_4" }, "set": { "born": {"$date": "1809"}, "country": "Italy" }, "cite": ["@o"] }
                 ] }),
        T,
    )
    .unwrap();
    let ids = |r: &Json| -> Vec<String> {
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap().to_owned())
            .collect()
    };
    // Ada born 1815-12-10, Babbage 1791-12-26, Menabrea 1809 (year precision).
    let r = g
        .find(&json!({ "target": "nodes", "where": { "all": [
            { "field": "type", "op": "eq", "value": "Person" },
            { "field": "prop:born", "op": "lt", "value": {"$date": "1810"} } ] },
            "fields": ["prop:born.year"] }))
        .unwrap();
    assert_eq!(ids(&r), ["n_2", "n_4"]);
    assert_eq!(r["rows"][1]["values"]["prop:born.year"], 1809);
    // [OP-FIND-4] interval semantics: 1809 is inside 1809, not before 1809-06.
    let r = g.find(&json!({ "target": "nodes", "where": { "field": "prop:born", "op": "eq", "value": {"$date": "1815"} } })).unwrap();
    assert_eq!(ids(&r), ["n_1"]);
    let r = g.find(&json!({ "target": "nodes", "where": { "field": "prop:born", "op": "lt", "value": {"$date": "1809-06"} } })).unwrap();
    assert_eq!(ids(&r), ["n_2"]);
    // Missing values satisfy only missing / ne / not_in.
    let r = g.find(&json!({ "target": "nodes", "where": { "field": "prop:country", "op": "ne", "value": "Italy" } })).unwrap();
    assert_eq!(ids(&r), ["n_1", "n_2", "n_3"]);
    let r = g.find(&json!({ "target": "nodes", "where": { "field": "name", "op": "contains", "value": "BABB" } })).unwrap();
    assert_eq!(ids(&r), ["n_2"]);
    // Computed values, linked, sorting with missing last.
    let r = g.find(&json!({ "target": "nodes", "where": { "field": "degree(WROTE_ABOUT, out)", "op": "gte", "value": 1 } })).unwrap();
    assert_eq!(ids(&r), ["n_1", "n_4"]);
    let r = g.find(&json!({ "target": "nodes", "where": { "linked": { "link_type": "DESIGNED", "direction": "out", "node": "n_3" } } })).unwrap();
    assert_eq!(ids(&r), ["n_2"]);
    let r = g.find(&json!({ "target": "nodes", "sort": [{ "by": "prop:born", "order": "desc" }], "limit": 2 })).unwrap();
    assert_eq!(ids(&r), ["n_1", "n_4"]);
    assert_eq!(r["total"], 4);
    assert_eq!(r["next_cursor"], "2");
    let r2 = g.find(&json!({ "target": "nodes", "sort": [{ "by": "prop:born", "order": "desc" }], "limit": 2, "cursor": "2" })).unwrap();
    assert_eq!(ids(&r2), ["n_2", "n_3"]);
    // Groups: links per type; aggregates over node properties.
    let r = g
        .find(&json!({ "target": "links", "group_by": ["type"] }))
        .unwrap();
    assert_eq!(
        r["groups"][0],
        json!({ "key": { "type": "WROTE_ABOUT" }, "count": 2 })
    );
    assert_eq!(r["total"], 3);
    let r = g
        .find(&json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "Person" },
                       "aggregate": [{ "fn": "count" }, { "fn": "avg", "of": "prop:born.year" },
                                     { "fn": "min", "of": "prop:born" }, { "fn": "count_distinct", "of": "prop:country" },
                                     { "fn": "median", "of": "degree" }] }))
        .unwrap();
    let g0 = &r["groups"][0];
    assert_eq!(g0["count"], 3);
    assert_eq!(
        g0["avg(prop:born.year)"],
        json!((1815.0 + 1791.0 + 1809.0) / 3.0)
    );
    assert_eq!(g0["min(prop:born)"], json!({ "$date": "1791-12-26" }));
    assert_eq!(g0["count_distinct(prop:country)"], 2);
    assert_eq!(g0["median(degree)"], json!(2.0));
    let r = g
        .find(&json!({ "target": "citations", "group_by": ["evidence"], "sort": [{ "by": "count", "order": "desc" }], "limit": 1 }))
        .unwrap();
    assert_eq!(r["groups"][0]["key"]["evidence"], "e_2");
    let e = g
        .find(
            &json!({ "target": "nodes", "where": { "field": "prop:x", "op": "near", "value": 1 } }),
        )
        .unwrap_err();
    assert_eq!(e.reason.code(), "find.invalid_field");
}

#[test]
fn naming_policy_checks_names_and_never_renames() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    // `styled` (the default): a name in another style fails and the error names the style.
    let e = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "person", "name": "Mary Somerville",
                                 "props": { "born": {"$date": "1780"} }, "cite": ["e_1"] }] }),
            T,
        )
        .unwrap_err();
    assert_eq!(e.reason.code(), "vocab.not_styled");
    assert_eq!(e.details["field"], "type");
    assert_eq!(e.details["expected"], "Person");
    let e = g
        .write(
            ACTOR,
            &json!({ "nodes": [{ "type": "Person", "name": "Mary Somerville",
                                 "props": { "bornOn": {"$date": "1780"} }, "cite": ["e_1"] }] }),
            T,
        )
        .unwrap_err();
    assert_eq!(e.details["expected"], "born_on");
    let e = g
        .write(ACTOR, &json!({ "links": [{ "type": "wrote about", "from": "n_2", "to": "n_3", "cite": ["e_1"] }] }), T)
        .unwrap_err();
    assert_eq!(e.details["expected"], "WROTE_ABOUT");
    // Reads check the same way instead of quietly matching nothing.
    for (e, expected) in [
        (
            g.find(&json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "person" } }))
                .unwrap_err(),
            "Person",
        ),
        (
            g.find(&json!({ "target": "nodes", "where": { "field": "prop:bornOn", "op": "exists" } }))
                .unwrap_err(),
            "born_on",
        ),
        (
            g.neighbors(&json!({ "start": ["n_3"], "link_types": ["wrote about"] }))
                .unwrap_err(),
            "WROTE_ABOUT",
        ),
        (
            g.search(&json!({ "query": "engine", "types": ["machine"] }))
                .unwrap_err(),
            "Machine",
        ),
    ] {
        assert_eq!(e.reason.code(), "vocab.not_styled");
        assert_eq!(e.details["expected"], expected);
    }

    // `free`: names are stored and matched exactly as given.
    let dir = tempfile::tempdir().unwrap();
    let mut g = new_graph(
        dir.path(),
        Policies {
            naming: mygraph_core::model::Naming::Free,
            ..Policies::default()
        },
    );
    g.write(
        ACTOR,
        &json!({ "evidence": [{ "ref": "@e", "kind": "text", "content": "x" }],
                 "nodes": [{ "type": "person", "name": "Ada", "props": { "bornOn": 1815 }, "cite": ["@e"] }] }),
        T,
    )
    .unwrap();
    assert_eq!(&*g.state().nodes[&1].type_, "person");
    let r = g
        .find(&json!({ "target": "nodes", "where": { "field": "prop:bornOn", "op": "eq", "value": 1815 },
                       "aggregate": [{ "fn": "count" }] }))
        .unwrap();
    assert_eq!(r["groups"][0]["count"], 1);
    let r = g
        .find(
            &json!({ "target": "nodes", "where": { "field": "prop:born_on", "op": "exists" },
                       "aggregate": [{ "fn": "count" }] }),
        )
        .unwrap();
    assert_eq!(r["groups"][0]["count"], 0);
    assert_eq!(
        g.write(
            ACTOR,
            &json!({ "nodes": [{ "type": "a person", "name": "B", "cite": ["e_1"] }] }),
            T
        )
        .unwrap_err()
        .reason
        .code(),
        "vocab.bad_char"
    );
}

#[test]
fn find_without_group_by_has_one_group_even_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    let g = traversal_graph(dir.path());
    let r = g
        .find(&json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "Planet" },
                       "aggregate": [{ "fn": "count" }, { "fn": "count_distinct", "of": "name" },
                                     { "fn": "max", "of": "prop:born" }, { "fn": "avg", "of": "degree" }] }))
        .unwrap();
    assert_eq!(r["total"], 1);
    assert_eq!(
        r["groups"],
        json!([{ "key": {}, "count": 0, "count_distinct(name)": 0, "max(prop:born)": null, "avg(degree)": null }])
    );
    let r = g
        .find(
            &json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "Planet" },
                       "group_by": ["name"] }),
        )
        .unwrap();
    assert_eq!(r["groups"], json!([]));
    // `ne` on a list is the negation of `eq`: no element equal.
    let mut g = g;
    g.write(
        ACTOR,
        &json!({ "updates": [{ "target": { "node": "n_1" }, "set": { "langs": ["en", "fr"] } }] }),
        T,
    )
    .unwrap();
    let n = |op: &str| {
        g.find(&json!({ "target": "nodes", "where": { "all": [
            { "field": "type", "op": "eq", "value": "Person" },
            { "field": "prop:langs", "op": op, "value": "en" }] }, "aggregate": [{ "fn": "count" }] }))
            .unwrap()["groups"][0]["count"]
            .clone()
    };
    assert_eq!(n("eq"), 1);
    assert_eq!(n("ne"), 2);
}

#[test]
fn find_uses_the_type_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    let count = |g: &Graph<GraphFile>, q: Json| g.find(&q).unwrap()["groups"][0]["count"].clone();
    assert_eq!(
        count(
            &g,
            json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "Person" }, "aggregate": [{ "fn": "count" }] })
        ),
        3
    );
    // Type plus another condition: candidates from the index, then the rest of the test.
    let q = json!({ "target": "nodes", "where": { "all": [
        { "field": "type", "op": "in", "value": ["Person", "Machine"] },
        { "field": "name", "op": "starts_with", "value": "a" } ] } });
    let ids: Vec<Json> = g.find(&q).unwrap()["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].clone())
        .collect();
    assert_eq!(ids, [json!("n_1"), json!("n_3")]);
    // The index follows retractions.
    g.retract(ACTOR, &json!({ "targets": ["n_4"] }), T).unwrap();
    assert_eq!(
        count(&g, json!({ "target": "nodes", "group_by": ["type"] })),
        2
    );
    assert_eq!(
        count(
            &g,
            json!({ "target": "nodes", "where": { "field": "type", "op": "eq", "value": "Person" }, "aggregate": [{ "fn": "count" }] })
        ),
        2
    );
}

#[test]
fn find_with_linked_starts_from_the_linked_node() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    let rows = |g: &Graph<GraphFile>, w: Json| {
        ids_of(&g.find(&json!({ "target": "nodes", "where": w })).unwrap()["rows"])
    };
    // Who wrote about the engine: alone (from the engine's links), with a type, and under `any`
    // (a scan) give the same nodes.
    let wrote =
        json!({ "linked": { "link_type": "WROTE_ABOUT", "direction": "out", "node": "n_3" } });
    assert_eq!(rows(&g, wrote.clone()), ["n_1", "n_4"]);
    let typed = json!({ "all": [{ "field": "type", "op": "eq", "value": "Person" }, wrote] });
    assert_eq!(rows(&g, typed.clone()), ["n_1", "n_4"]);
    assert_eq!(rows(&g, json!({ "any": [wrote] })), ["n_1", "n_4"]);
    // Direction is the candidate's: what Ada wrote about.
    assert_eq!(
        rows(
            &g,
            json!({ "linked": { "link_type": "WROTE_ABOUT", "direction": "in", "node": "n_1" } })
        ),
        ["n_3"]
    );
    // No link type: the undirected collaboration counts, Babbage's DESIGNED link points away.
    assert_eq!(
        rows(
            &g,
            json!({ "linked": { "direction": "out", "node": "n_2" } })
        ),
        ["n_1"]
    );
    // Retracted links and nodes are not candidates.
    g.retract(ACTOR, &json!({ "targets": ["l_4"] }), T).unwrap();
    assert_eq!(rows(&g, typed.clone()), ["n_1"]);
    g.retract(ACTOR, &json!({ "targets": ["n_1"] }), T).unwrap();
    assert_eq!(rows(&g, typed), Vec::<String>::new());
}

#[test]
fn traversal_link_type_filter_follows_writes_retractions_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = traversal_graph(dir.path());
    let reach = |g: &Graph<GraphFile>, q: Json| ids_of(&g.neighbors(&q).unwrap()["nodes"]);
    let inspired = json!({ "start": ["n_1"], "link_types": ["INSPIRED"] });
    // A link type no link has yet: nothing to follow.
    assert_eq!(reach(&g, inspired.clone()), ["n_1"]);
    let res = g
        .write(
            ACTOR,
            &json!({ "evidence": [{ "ref": "@o", "kind": "observation", "content": "later" }],
                     "links": [{ "type": "INSPIRED", "from": "n_1", "to": "n_4", "cite": ["@o"] }] }),
            T,
        )
        .unwrap();
    let link = res["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["section"] == "links")
        .unwrap()["id"]
        .clone();
    assert_eq!(reach(&g, inspired.clone()), ["n_1", "n_4"]);
    let p = g
        .paths(&json!({ "from": "n_1", "to": "n_4", "link_types": ["INSPIRED"] }))
        .unwrap();
    assert_eq!(p["paths"][0]["length"], 1);
    g.retract(ACTOR, &json!({ "targets": [link] }), T).unwrap();
    assert_eq!(reach(&g, inspired), ["n_1"]);
    // After reopening, the numbers are rebuilt from the file.
    drop(g);
    let g = open(dir.path());
    assert_eq!(
        reach(
            &g,
            json!({ "start": ["n_3"], "link_types": ["WROTE_ABOUT"], "direction": "in" })
        ),
        ["n_3", "n_1", "n_4"]
    );
}
