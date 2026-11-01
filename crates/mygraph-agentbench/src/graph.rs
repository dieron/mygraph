//! The graph side shared by the benchmarks: the skill as system prompt, a fresh graph, the MCP
//! server entry for an agent, and the evidence an agent stored.

use crate::agent::Result;
use mygraph_core::engine::Graph;
use mygraph_core::ids::ObjectKind;
use mygraph_core::model::{Policies, Status, id_string};
use mygraph_store_sqlite::GraphFile;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SKILL: &str = include_str!("../../../skills/mygraph/SKILL.md");
const SKILL_WRITE: &str = include_str!("../../../skills/mygraph/references/write.md");
const SKILL_QUERY: &str = include_str!("../../../skills/mygraph/references/query.md");
const GRAPH: &str = "default";

/// The system prompt of agents that use the graph: the skill and its references.
pub fn system() -> String {
    let body = |s: &'static str| s.splitn(3, "---\n").nth(2).unwrap_or(s);
    format!(
        "You are an assistant that keeps knowledge in MyGraph, a knowledge graph where every fact \
         cites its evidence, through the `mygraph` MCP tools. This is your guide to it, followed \
         by its references for writing and querying.\n\n{}\n\n{SKILL_WRITE}\n\n{SKILL_QUERY}",
        body(SKILL)
    )
}

/// A data folder with an empty graph, replacing whatever was there.
pub fn create(data: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(data);
    fs::create_dir_all(data)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    drop(GraphFile::create(data, GRAPH, &Policies::default(), now)?);
    Ok(())
}

/// The `mcpServers` map for an agent working on the graph in `data`.
pub fn server(mygraph: &Path, data: &Path, actor: &str) -> Json {
    json!({ "mygraph": { "command": mygraph, "args": [
        "mcp", "--data-dir", data, "--graph", GRAPH, "--actor", actor
    ] } })
}

/// An evidence as stored: its kind (`text`, `url`, `conversation`, …) and text.
pub struct Evidence {
    pub kind: String,
    pub content: String,
}

/// Every active evidence in the graph, by ID, and the graph's size.
pub fn contents(data: &Path) -> Result<(BTreeMap<String, Evidence>, Json)> {
    let file = GraphFile::open(data, GRAPH)?;
    let policies = file.policies()?;
    let g = Graph::open(file, GRAPH, policies)?;
    let st = g.state();
    let ids: Vec<i64> = st
        .evidence
        .iter()
        .filter(|(_, e)| e.status == Status::Active)
        .map(|(i, _)| i)
        .collect();
    let mut texts = BTreeMap::new();
    for chunk in ids.chunks(100) {
        let refs: Vec<String> = chunk
            .iter()
            .map(|i| id_string(ObjectKind::Evidence, *i))
            .collect();
        let r = g
            .get(&json!({ "refs": refs, "detail": "brief", "include": {"content": true} }))
            .map_err(|e| e.to_json().to_string())?;
        for ((idx, id), item) in chunk
            .iter()
            .zip(refs)
            .zip(r["items"].as_array().into_iter().flatten())
        {
            let kind = st.evidence.get(idx).map(|e| e.kind.clone());
            texts.insert(
                id,
                Evidence {
                    kind: kind.unwrap_or_default(),
                    content: item["content"].as_str().unwrap_or_default().to_owned(),
                },
            );
        }
    }
    let size = json!({
        "nodes": st.nodes.iter().filter(|(_, n)| n.status == Status::Active).count(),
        "links": st.links.iter().filter(|(_, l)| l.status == Status::Active).count(),
        "evidence": ids.len(),
        "citations": st.citations.iter().filter(|(_, c)| c.status == Status::Active).count(),
    });
    Ok((texts, size))
}

/// A copy of a data folder, for an agent that should not change the original.
pub fn copy(from: &Path, to: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(to);
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
