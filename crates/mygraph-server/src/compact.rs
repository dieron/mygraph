//! The compact text format — spec/api/compact-format.md.

use serde_json::{Map, Value as Json};
use std::fmt::Write;

/// [CMP-1] One value.
pub fn val(v: &Json) -> String {
    match v {
        Json::String(_) | Json::Number(_) | Json::Bool(_) | Json::Null => v.to_string(),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(val).collect();
            format!("[{}]", parts.join(", "))
        }
        Json::Object(m) => match (m.len(), m.get("$date").or_else(|| m.get("$datetime"))) {
            (1, Some(Json::String(s))) => s.clone(),
            _ => v.to_string(),
        },
    }
}

fn s<'a>(o: &'a Json, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Json::as_str)
}

/// [CMP-2] An object's brief form, recognised by its ID's prefix.
pub fn brief(o: &Json) -> String {
    let id = s(o, "id").unwrap_or("?");
    let q = |k: &str| s(o, k).map(|x| Json::from(x).to_string());
    let mut out = match id.split('_').next().unwrap_or("") {
        "n" => format!(
            "{id} {} {}",
            s(o, "type").unwrap_or("?"),
            q("name").unwrap_or_default()
        ),
        "l" => {
            let arrow = if o.get("directed") == Some(&Json::Bool(false)) {
                "--"
            } else {
                "->"
            };
            // With `ends` (get), each end also shows its name.
            let end = |side: &str| {
                let id = s(o, side).unwrap_or("?");
                match s(&o["ends"][side], "name") {
                    Some(n) => format!("{id} {}", Json::from(n)),
                    None => id.to_owned(),
                }
            };
            format!(
                "{id} {} {} {arrow} {}",
                s(o, "type").unwrap_or("?"),
                end("from"),
                end("to")
            )
        }
        "e" => {
            let kind = s(o, "evidence_kind")
                .or_else(|| s(o, "kind"))
                .unwrap_or("?");
            match q("title").or_else(|| q("uri")) {
                Some(t) => format!("{id} {kind} {t}"),
                None => format!("{id} {kind}"),
            }
        }
        "c" => format!(
            "{id} {} {} -> {}",
            s(o, "role").unwrap_or("?"),
            s(o, "evidence").unwrap_or("?"),
            s(o, "target").unwrap_or("?")
        ),
        _ => id.to_owned(),
    };
    if let Some(st) = s(o, "status").filter(|st| *st != "active") {
        let _ = write!(out, " [{st}]");
    }
    out
}

/// [CMP-3]
fn props(m: &Map<String, Json>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}={}", val(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Members whose default value says nothing.
fn quiet(k: &str, v: &Json) -> bool {
    v.is_null()
        || v.as_array().is_some_and(Vec::is_empty)
        || (v == &Json::Bool(false)
            && matches!(
                k,
                "truncated" | "more" | "dry_run" | "types_truncated" | "multi"
            ))
}

/// [CMP-4] The generic layout for the members of `o` not in `skip`.
fn generic(out: &mut String, o: &Json, skip: &[&str], indent: usize) {
    let pad = " ".repeat(indent);
    let Some(m) = o.as_object() else {
        let _ = writeln!(out, "{pad}{}", val(o));
        return;
    };
    for (k, v) in m {
        if skip.contains(&k.as_str()) || quiet(k, v) {
            continue;
        }
        if k == "warnings" {
            warnings(out, v, indent);
            continue;
        }
        match v {
            Json::Object(inner) if !is_scalar_object(v) => {
                if inner.is_empty() {
                    continue;
                }
                let _ = writeln!(out, "{pad}{k}:");
                generic(out, v, &[], indent + 2);
            }
            Json::Array(items) if items.iter().any(|i| i.is_object() && !is_scalar_object(i)) => {
                let _ = writeln!(out, "{pad}{k}:");
                for i in items {
                    if i.get("id").is_some() {
                        let _ = writeln!(out, "{pad}- {}", brief(i));
                    } else {
                        let mut item = String::new();
                        generic(&mut item, i, &[], 0);
                        let _ = writeln!(out, "{pad}- {}", item.trim_end().replace('\n', "; "));
                    }
                }
            }
            _ => {
                let _ = writeln!(out, "{pad}{k}: {}", val(v));
            }
        }
    }
}

/// `{"$date": …}` and the like render as one value.
fn is_scalar_object(v: &Json) -> bool {
    v.as_object()
        .is_some_and(|m| m.len() == 1 && (m.contains_key("$date") || m.contains_key("$datetime")))
}

fn warnings(out: &mut String, v: &Json, indent: usize) {
    let pad = " ".repeat(indent);
    // New vocabulary without similar existing names is expected (an empty graph has only that):
    // one line lists it all, so a warning that matters is not buried.
    let plain = |w: &Json| {
        s(w, "code") == Some("NEW_VOCABULARY")
            && w["details"]["similar"].as_array().is_none_or(Vec::is_empty)
    };
    let new: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| plain(w))
        .map(|w| {
            let kind = s(&w["details"], "kind").unwrap_or("").replace('_', " ");
            format!("{} ({kind})", s(&w["details"], "name").unwrap_or("?"))
        })
        .collect();
    if !new.is_empty() {
        let _ = writeln!(out, "{pad}new vocabulary: {}", new.join(", "));
    }
    for w in v.as_array().into_iter().flatten().filter(|w| !plain(w)) {
        let _ = write!(
            out,
            "{pad}warning {}: {}",
            s(w, "code").unwrap_or("?"),
            s(w, "message").unwrap_or("")
        );
        match w.get("details") {
            Some(d) if !d.is_null() && d.as_object().is_none_or(|m| !m.is_empty()) => {
                let _ = writeln!(out, " {d}");
            }
            _ => out.push('\n'),
        }
    }
}

fn items<'a>(v: &'a Json, k: &str) -> impl Iterator<Item = &'a Json> {
    v.get(k).and_then(Json::as_array).into_iter().flatten()
}

fn joined<'a>(v: Option<&'a Json>, f: impl Fn(&'a str, &'a Json) -> String) -> String {
    v.and_then(Json::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| f(k, v))
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default()
}

/// The compact rendering of operation `op`'s response `r` to request `req`.
pub fn render(op: &str, req: &Json, r: &Json) -> String {
    let mut out = String::new();
    let skip: &[&str] = match op {
        "overview" => {
            overview(&mut out, r);
            &[
                "graph",
                "spec_version",
                "last_seq",
                "counts",
                "node_types",
                "link_types",
                "evidence_kinds",
                "policies",
                "recent",
                "conventions",
                "type_detail",
            ]
        }
        "search" => {
            if items(r, "hits").next().is_none() {
                out.push_str(
                    "no hits
",
                );
            }
            for h in items(r, "hits") {
                let score = h.get("score").and_then(Json::as_f64).unwrap_or(0.0);
                let _ = write!(
                    out,
                    "{score:.2} {} ({})",
                    brief(h),
                    s(h, "matched").unwrap_or("")
                );
                if let Some(sn) = s(h, "snippet") {
                    let _ = write!(out, " {}", sn.replace('\n', " "));
                }
                out.push('\n');
            }
            &["hits"]
        }
        "resolve" => {
            let names: Vec<&Json> = items(req, "candidates").collect();
            for (i, res) in items(r, "results").enumerate() {
                let name = names
                    .get(i)
                    .and_then(|c| c.get("name"))
                    .map(val)
                    .unwrap_or_default();
                let _ = write!(out, "{} {name} {}", i + 1, s(res, "verdict").unwrap_or("?"));
                let best = res.get("best");
                if let Some(b) = best {
                    let _ = write!(out, " {}", scored(b));
                }
                out.push('\n');
                for c in items(res, "candidates") {
                    if best.and_then(|b| b.get("id")) != c.get("id") {
                        let _ = writeln!(out, "  or {}", scored(c));
                    }
                }
            }
            &["results"]
        }
        "get" => {
            for it in items(r, "items") {
                get_item(&mut out, it);
            }
            &["items"]
        }
        "write" => {
            let mut head = vec![format!("batch {}", s(r, "batch").unwrap_or("none"))];
            if r.get("dry_run") == Some(&Json::Bool(true)) {
                head.push("dry run".into());
            }
            for what in ["created", "updated", "matched"] {
                let c = joined(r["counts"].get(what), |k, v| format!("{k} {v}"));
                if !c.is_empty() {
                    head.push(format!("{what} {c}"));
                }
            }
            let _ = writeln!(out, "{}", head.join(" · "));
            let refs = joined(r.get("refs"), |k, v| {
                format!("{k}={}", v.as_str().unwrap_or("?"))
            });
            if !refs.is_empty() {
                let _ = writeln!(out, "refs: {refs}");
            }
            let other: Vec<String> = items(r, "items")
                .filter(|i| !matches!(s(i, "outcome"), Some("created" | "matched" | "unchanged")))
                .map(|i| {
                    format!(
                        "{}#{} {} {}",
                        s(i, "section").unwrap_or("?"),
                        i["index"],
                        s(i, "outcome").unwrap_or("?"),
                        s(i, "id").unwrap_or("")
                    )
                })
                .collect();
            if !other.is_empty() {
                let _ = writeln!(out, "items: {}", other.join(" · "));
            }
            &["batch", "dry_run", "counts", "refs", "items"]
        }
        "retract" => {
            let _ = writeln!(out, "batch {}", s(r, "batch").unwrap_or("none"));
            for i in items(r, "items") {
                let _ = write!(
                    out,
                    "{} {}",
                    s(i, "id").unwrap_or("?"),
                    s(i, "outcome").unwrap_or("?")
                );
                if let Some(c) = s(i, "cascade_of") {
                    let _ = write!(out, " (cascade of {c})");
                }
                out.push('\n');
            }
            &["batch", "items"]
        }
        "neighbors" => {
            for n in items(r, "nodes") {
                let _ = write!(out, "{}", brief(n));
                if let Some(h) = n.get("hop") {
                    let _ = write!(out, " (hop {h})");
                }
                if let Some(src) = n.get("sources") {
                    let _ = write!(out, " src {src}");
                }
                out.push('\n');
            }
            for l in items(r, "links") {
                let _ = write!(out, "{}", brief(l));
                if let Some(src) = l.get("sources") {
                    let _ = write!(out, " src {src}");
                }
                out.push('\n');
            }
            &["nodes", "links"]
        }
        "paths" => {
            if items(r, "paths").next().is_none() {
                out.push_str(
                    "no path found (active links only, within max_length)
",
                );
            }
            for p in items(r, "paths") {
                path(&mut out, p);
            }
            &["paths"]
        }
        "find" => {
            for row in items(r, "rows") {
                let _ = write!(out, "{}", brief(row));
                if let Some(v) = row.get("values").and_then(Json::as_object) {
                    let _ = write!(out, " · {}", props(v));
                }
                out.push('\n');
            }
            for g in items(r, "groups") {
                let key = g
                    .get("key")
                    .and_then(Json::as_object)
                    .map(props)
                    .unwrap_or_default();
                let aggs: Vec<String> = g
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| *k != "key")
                    .map(|(k, v)| format!("{k}={}", val(v)))
                    .collect();
                let _ = writeln!(out, "{{{key}}} {}", aggs.join(" "));
            }
            &["rows", "groups"]
        }
        "history" => {
            for e in items(r, "entries") {
                let _ = write!(
                    out,
                    "{} {} {} {} {} {}",
                    e["seq"],
                    s(e, "at").unwrap_or(""),
                    s(e, "actor").unwrap_or(""),
                    s(e, "batch").unwrap_or(""),
                    s(e, "op").unwrap_or(""),
                    s(e, "object").unwrap_or("-")
                );
                if let Some(d) = e.get("diff").filter(|d| !d.is_null()) {
                    let _ = write!(out, " {d}");
                }
                out.push('\n');
            }
            &["entries"]
        }
        _ => &[],
    };
    generic(&mut out, r, skip, 0);
    if out.is_empty() {
        out.push_str("ok\n");
    }
    out
}

/// A resolve candidate: brief, score and reasons.
fn scored(c: &Json) -> String {
    let why: Vec<&str> = items(c, "why").filter_map(Json::as_str).collect();
    let score = c.get("score").and_then(Json::as_f64).unwrap_or(0.0);
    format!("{} {score:.2} ({})", brief(c), why.join("; "))
}

fn get_item(out: &mut String, it: &Json) {
    if let Some(e) = it.get("error") {
        let _ = writeln!(
            out,
            "{}: error {}: {}",
            val(it.get("ref").unwrap_or(&Json::Null)),
            s(e, "code").unwrap_or("?"),
            s(e, "message").unwrap_or("")
        );
        return;
    }
    let mut head = brief(it);
    if let Some(m) = s(it, "merged_from") {
        let _ = write!(head, " (merged from {m})");
    }
    let aliases: Vec<String> = items(it, "aliases").map(val).collect();
    if !aliases.is_empty() {
        let _ = write!(head, " aka {}", aliases.join(", "));
    }
    if let Some(p) = it
        .get("props")
        .and_then(Json::as_object)
        .filter(|p| !p.is_empty())
    {
        let _ = write!(head, " · {}", props(p));
    }
    if let Some(sup) = it.get("support").and_then(Json::as_object) {
        let roles: Vec<String> = sup
            .iter()
            .filter(|(k, _)| *k != "sources")
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        let _ = write!(
            head,
            " · sources {} ({})",
            sup.get("sources").unwrap_or(&Json::from(0)),
            roles.join(", ")
        );
    }
    let _ = writeln!(out, "{head}");
    let include_evidence = it.get("evidence").is_some_and(Json::is_object);
    let mut skip = vec![
        "id",
        "type",
        "name",
        "status",
        "aliases",
        "props",
        "support",
        "links",
        "content",
        "content_length",
        "content_truncated",
        "merged_from",
        "title",
        "uri",
        "from",
        "to",
        "directed",
        "role",
        "target",
        "ends",
    ];
    // A citation's `evidence` is part of its brief form; a node's is the evidence list.
    skip.push("evidence");
    if s(it, "id").is_some_and(|id| id.starts_with("e_")) {
        skip.push("kind");
    }
    generic(out, it, &skip, 2);
    if let Some(links) = it.get("links") {
        let counts: Vec<String> = items(links, "counts")
            .map(|c| {
                format!(
                    "{} {} {}",
                    s(c, "type").unwrap_or("?"),
                    s(c, "direction").unwrap_or("?"),
                    c["count"]
                )
            })
            .collect();
        if !counts.is_empty() {
            let _ = writeln!(out, "  links: {}", counts.join(" · "));
        }
        for l in items(links, "items") {
            let arrow = match s(l, "direction") {
                Some("out") => "->",
                Some("in") => "<-",
                _ => "--",
            };
            let other = l.get("other").map(brief).unwrap_or_default();
            let _ = writeln!(
                out,
                "  {arrow} {} {other} ({})",
                s(l, "type").unwrap_or("?"),
                s(l, "id").unwrap_or("?")
            );
        }
        if links.get("truncated") == Some(&Json::Bool(true)) {
            out.push_str(
                "  … more links (truncated; raise include.links.limit or filter by types)\n",
            );
        }
    }
    if include_evidence {
        let ev = &it["evidence"];
        for e in items(ev, "items") {
            let _ = write!(
                out,
                "  evidence: {} {}",
                brief(e),
                s(e, "role").unwrap_or("")
            );
            if let Some(c) = s(e, "citation") {
                let _ = write!(out, " ({c})");
            }
            if let Some(p) = s(e, "prop") {
                let _ = write!(out, " prop {p}");
            }
            if let Some(q) = e.get("quote").filter(|q| q.is_string()) {
                let _ = write!(out, " {q}");
            }
            out.push('\n');
        }
        if ev.get("truncated") == Some(&Json::Bool(true)) {
            out.push_str("  … more evidence (truncated)\n");
        }
    }
    if let Some(c) = it.get("content").filter(|c| !c.is_null()) {
        let mut label = format!("  content ({} chars", it["content_length"]);
        if it.get("content_truncated") == Some(&Json::Bool(true)) {
            label.push_str(", truncated");
        }
        let _ = writeln!(out, "{label}):");
        let _ = writeln!(out, "{}", c.as_str().unwrap_or(""));
    }
}

fn path(out: &mut String, p: &Json) {
    let nodes: Vec<&Json> = items(p, "nodes").collect();
    let links: Vec<&Json> = items(p, "links").collect();
    let node = |n: &Json| {
        format!(
            "{} {}",
            s(n, "id").unwrap_or("?"),
            Json::from(s(n, "name").unwrap_or(""))
        )
    };
    let mut line = format!("{}:", p["length"]);
    if let Some(first) = nodes.first() {
        let _ = write!(line, " {}", node(first));
    }
    for (i, l) in links.iter().enumerate() {
        let from = nodes.get(i).and_then(|n| s(n, "id"));
        let t = s(l, "type").unwrap_or("?");
        let step = if l.get("directed") == Some(&Json::Bool(false)) {
            format!("-[{t}]-")
        } else if s(l, "from") == from {
            format!("-[{t}]->")
        } else {
            format!("<-[{t}]-")
        };
        let _ = write!(line, " {step}");
        if let Some(n) = nodes.get(i + 1) {
            let _ = write!(line, " {}", node(n));
        }
    }
    if let Some(m) = p.get("min_sources") {
        let _ = write!(line, " · min sources {m}");
    }
    let _ = writeln!(out, "{line}");
}

fn overview(out: &mut String, r: &Json) {
    let _ = writeln!(
        out,
        "graph {} · spec {} · last_seq {}",
        s(r, "graph").unwrap_or("?"),
        s(r, "spec_version").unwrap_or("?"),
        r["last_seq"]
    );
    let _ = writeln!(
        out,
        "counts: {}",
        joined(r.get("counts"), |k, v| format!("{k} {v}"))
    );
    for (label, key) in [("node types", "node_types"), ("link types", "link_types")] {
        let list: Vec<String> = items(r, key)
            .map(|t| {
                let mut e = format!("{} {}", s(t, "type").unwrap_or("?"), t["count"]);
                if t.get("symmetric") == Some(&Json::Bool(true)) {
                    e.push_str(" (symmetric)");
                }
                if let Some(d) = s(t, "description") {
                    let _ = write!(e, " ({})", Json::from(d));
                }
                e
            })
            .collect();
        let _ = writeln!(
            out,
            "{label}: {}",
            if list.is_empty() {
                "none".into()
            } else {
                list.join(" · ")
            }
        );
    }
    if r.get("types_truncated") == Some(&Json::Bool(true)) {
        out.push_str("types truncated: raise types_limit\n");
    }
    let kinds = joined(r.get("evidence_kinds"), |k, v| format!("{k} {v}"));
    let _ = writeln!(
        out,
        "evidence kinds: {}",
        if kinds.is_empty() {
            "none".into()
        } else {
            kinds
        }
    );
    if let Some(p) = r.get("policies").and_then(Json::as_object) {
        let _ = writeln!(out, "policies: {}", props(p));
    }
    let recent: Vec<&Json> = items(r, "recent").collect();
    if !recent.is_empty() {
        out.push_str("recent:\n");
        for b in recent {
            let _ = write!(
                out,
                "  {} {} {} {} {} changes",
                s(b, "batch").unwrap_or("?"),
                s(b, "at").unwrap_or(""),
                s(b, "actor").unwrap_or(""),
                s(b, "op").unwrap_or(""),
                b["changes"]
            );
            if let Some(n) = b.get("note").filter(|n| n.is_string()) {
                let _ = write!(out, " {n}");
            }
            out.push('\n');
        }
    }
    if let Some(c) = s(r, "conventions") {
        let _ = writeln!(out, "conventions: {c}");
    }
    if let Some(d) = r.get("type_detail") {
        let mut head = format!(
            "type {} ({}) · {} active · sample {}",
            s(d, "type").unwrap_or("?"),
            s(d, "kind").unwrap_or("?"),
            d["count"],
            d["sample"]
        );
        if d.get("symmetric") == Some(&Json::Bool(true)) {
            head.push_str(" · symmetric");
        }
        if let Some(desc) = s(d, "description") {
            let _ = write!(head, " · {}", Json::from(desc));
        }
        let _ = writeln!(out, "{head}");
        let pr: Vec<String> = items(d, "props")
            .map(|p| {
                let kinds: Vec<&str> = items(p, "kinds").filter_map(Json::as_str).collect();
                format!(
                    "{} {} {}",
                    s(p, "key").unwrap_or("?"),
                    kinds.join("|"),
                    p["fill"]
                )
            })
            .collect();
        if !pr.is_empty() {
            let _ = writeln!(out, "  props (key kinds fill): {}", pr.join(" · "));
        }
        for l in items(d, "links") {
            let others: Vec<String> = items(l, "other_types")
                .map(|o| format!("{} {}", s(o, "type").unwrap_or("?"), o["count"]))
                .collect();
            let _ = writeln!(
                out,
                "  {} {} {} → {}",
                s(l, "direction").unwrap_or("?"),
                s(l, "type").unwrap_or("?"),
                l["count"],
                others.join(", ")
            );
        }
        for e in items(d, "endpoints") {
            let _ = writeln!(
                out,
                "  {} -> {} {}",
                s(e, "from").unwrap_or("?"),
                s(e, "to").unwrap_or("?"),
                e["count"]
            );
        }
        let ex: Vec<String> = items(d, "examples").map(brief).collect();
        if !ex.is_empty() {
            let _ = writeln!(out, "  examples: {}", ex.join(" · "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_and_briefs() {
        assert_eq!(val(&json!({"$date": "1815-12-10"})), "1815-12-10");
        assert_eq!(val(&json!(["a", 1])), "[\"a\", 1]");
        assert_eq!(
            brief(&json!({"id": "n_1", "type": "Person", "name": "Ada", "status": "retracted"})),
            "n_1 Person \"Ada\" [retracted]"
        );
        assert_eq!(
            brief(
                &json!({"id": "l_1", "type": "KNOWS", "from": "n_1", "to": "n_2", "directed": false})
            ),
            "l_1 KNOWS n_1 -- n_2"
        );
    }

    #[test]
    fn paths_follow_link_direction() {
        let r = json!({"paths": [{"length": 2,
            "nodes": [{"id": "n_1", "name": "A"}, {"id": "n_2", "name": "B"}, {"id": "n_3", "name": "C"}],
            "links": [{"id": "l_1", "type": "X", "from": "n_1", "to": "n_2", "directed": true},
                      {"id": "l_2", "type": "Y", "from": "n_3", "to": "n_2", "directed": true}],
            "min_sources": 1}], "truncated": false});
        assert_eq!(
            render("paths", &json!({}), &r),
            "2: n_1 \"A\" -[X]-> n_2 \"B\" <-[Y]- n_3 \"C\" · min sources 1\n"
        );
    }

    #[test]
    fn nothing_is_dropped() {
        let r = json!({"hits": [], "truncated": true, "next_cursor": "20", "warnings": [{"code": "TRUNCATED", "message": "m", "details": null}]});
        assert_eq!(
            render("search", &json!({}), &r),
            "no hits\nnext_cursor: \"20\"\ntruncated: true\nwarning TRUNCATED: m\n"
        );
    }
}
