//! Writes an LDBC SNB archive as input for `neo4j-admin database import full`, with the same
//! mapping as the MyGraph load ([`crate::schema`]), so both databases hold the same graph.
//!
//! - Nodes: label = MyGraph type plus subtype labels; properties `key` (the import ID), `name` and
//!   the same properties with the same values as in MyGraph.
//! - Relationships: the MyGraph link types. Neo4j relationships are always directed, so `KNOWS`
//!   is stored once per pair, as listed, and queried without a direction.

use crate::archive::{Result, each_table, rows};
use crate::schema::{self, Kind, kind};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// One output file and the number of rows written to it.
struct Out {
    w: BufWriter<File>,
    rows: u64,
}

struct Files {
    dir: PathBuf,
    /// File name → open file; `BTreeMap` so the argument file lists them in a fixed order.
    open: BTreeMap<String, Out>,
}

impl Files {
    /// The file `name`, created with `header` on first use (a table may span several parts).
    fn get(&mut self, name: &str, header: impl FnOnce() -> String) -> Result<&mut Out> {
        if !self.open.contains_key(name) {
            let mut w = BufWriter::with_capacity(1 << 20, File::create(self.dir.join(name))?);
            writeln!(w, "{}", header())?;
            self.open.insert(name.to_owned(), Out { w, rows: 0 });
        }
        Ok(self.open.get_mut(name).expect("just inserted"))
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn neo4j_type(k: Kind) -> &'static str {
    match k {
        Kind::Str => "string",
        Kind::Int => "long",
        Kind::Date => "date",
        Kind::DateTime => "datetime",
    }
}

/// A CSV cell for a MyGraph property value; absent → empty cell (no property in Neo4j).
fn cell(v: Option<&Json>) -> String {
    match v {
        None => String::new(),
        Some(Json::String(s)) => quote(s),
        Some(Json::Number(n)) => n.to_string(),
        Some(Json::Array(a)) => quote(
            &a.iter()
                .map(|x| x.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(";"),
        ),
        Some(Json::Object(o)) => o
            .get("$datetime")
            .or_else(|| o.get("$date"))
            .and_then(Json::as_str)
            .map(str::to_owned)
            .unwrap_or_default(),
        Some(other) => quote(&other.to_string()),
    }
}

fn id_of(table: &str, id: &str) -> String {
    quote(&format!("{table}/{id}"))
}

pub fn export(archive: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let lists = crate::archive::person_lists(archive)?;
    let mut files = Files {
        dir: out.to_owned(),
        open: BTreeMap::new(),
    };
    let mut rels: BTreeMap<String, &'static str> = BTreeMap::new();
    each_table(archive, |_, table, r| {
        if let Some((_, spec)) = schema::node_table(table) {
            let (header, rows) = rows(r)?;
            let cols = spec.columns(&header)?;
            let fks = spec
                .fks
                .iter()
                .map(|fk| Ok((crate::archive::col(&header, fk.col)?, fk)))
                .collect::<Result<Vec<_>>>()?;
            let node_file = format!("nodes_{}.csv", spec.table);
            for row in rows {
                let row = row?;
                let n = spec.row(&cols, &row, &lists)?;
                let f = files.get(&node_file, || {
                    let mut h = vec![
                        format!("key:ID({})", spec.table),
                        ":LABEL".into(),
                        "name".into(),
                    ];
                    h.extend(
                        cols.props
                            .iter()
                            .map(|(_, p)| format!("{p}:{}", neo4j_type(kind(p)))),
                    );
                    h.extend(
                        spec.list_props()
                            .iter()
                            .map(|(_, p)| format!("{p}:string[]")),
                    );
                    h.join(",")
                })?;
                let labels = std::iter::once(spec.ty.to_owned())
                    .chain(n.labels)
                    .collect::<Vec<_>>()
                    .join(";");
                let mut line = vec![quote(&n.key), labels, quote(&n.name)];
                line.extend(cols.props.iter().map(|(_, p)| cell(n.props.get(p))));
                line.extend(spec.list_props().iter().map(|(_, p)| cell(n.props.get(*p))));
                writeln!(f.w, "{}", line.join(","))?;
                f.rows += 1;
                // Foreign keys: one relationship per non-empty column.
                for &(c, fk) in &fks {
                    if row[c].is_empty() {
                        continue;
                    }
                    let (mut a, mut b) =
                        ((spec.table, n.id.to_string()), (fk.target, row[c].clone()));
                    if fk.reverse {
                        std::mem::swap(&mut a, &mut b);
                    }
                    let name = format!("rels_{}_{}_{}.csv", fk.link, a.0, b.0);
                    rels.insert(name.clone(), fk.link);
                    let f = files.get(&name, || format!(":START_ID({}),:END_ID({})", a.0, b.0))?;
                    writeln!(f.w, "{},{}", id_of(a.0, &a.1), id_of(b.0, &b.1))?;
                    f.rows += 1;
                }
            }
        } else if let Some(spec) = schema::edge_table(table) {
            let (header, rows) = rows(r)?;
            let name = format!("rels_{}_{}_{}.csv", spec.link, spec.from, spec.to);
            rels.insert(name.clone(), spec.link);
            for row in rows {
                let row = row?;
                let f = files.get(&name, || {
                    let mut h = vec![
                        format!(":START_ID({})", spec.from),
                        format!(":END_ID({})", spec.to),
                    ];
                    h.extend(
                        header[2..]
                            .iter()
                            .map(|p| format!("{p}:{}", neo4j_type(kind(p)))),
                    );
                    h.join(",")
                })?;
                let mut line = vec![id_of(spec.from, &row[0]), id_of(spec.to, &row[1])];
                line.extend(
                    header[2..]
                        .iter()
                        .zip(&row[2..])
                        .map(|(p, v)| cell(schema::value(p, v).as_ref())),
                );
                writeln!(f.w, "{}", line.join(","))?;
                f.rows += 1;
            }
        }
        Ok(())
    })?;

    // Arguments for `neo4j-admin database import full`, one per line. The Windows script does not
    // expand `@file`, so a caller passes the lines as arguments. Paths are absolute and in the
    // platform's own form: neo4j-admin rejects relative names and reads `C:/…` as a URI scheme.
    let mut args = vec!["--overwrite-destination=true".to_owned()];
    let mut counts = serde_json::Map::new();
    for (name, f) in &mut files.open {
        f.w.flush()?;
        let path = std::path::absolute(out.join(name))?;
        match rels.get(name) {
            Some(link) => args.push(format!("--relationships={link}={}", path.display())),
            None => args.push(format!("--nodes={}", path.display())),
        }
        counts.insert(name.clone(), json!(f.rows));
    }
    std::fs::write(out.join("import.args"), args.join("\n") + "\n")?;
    println!("{}", serde_json::to_string_pretty(&Json::Object(counts))?);
    Ok(())
}
