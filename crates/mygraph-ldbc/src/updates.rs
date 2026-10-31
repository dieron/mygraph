//! The update streams of LDBC SNB Interactive v1 (`updateStream_0_0_person.csv`,
//! `updateStream_0_0_forum.csv`): the eight insert operations, turned into the nodes and links a
//! load of the same rows would create ([`crate::schema`]), as one MyGraph `write` or one Neo4j
//! transaction of `MERGE`s, so both stay idempotent and hold the same graph afterwards.
//!
//! A row is `scheduled time|dependency time|operation|fields…`; times are milliseconds since the
//! epoch. Operations: 1 person, 2 like of a post, 3 like of a comment, 4 forum, 5 membership,
//! 6 post, 7 comment, 8 friendship.

use crate::archive::Result;
use crate::schema::{self, Kind, NodeRow, NodeTable, PersonLists, kind};
use serde_json::{Map, Value as Json, json};
use std::io::{BufRead, BufReader};
use std::path::Path;

pub struct Event {
    pub time: i64,
    pub op: u8,
    pub fields: Vec<String>,
}

pub const OP_NAMES: [&str; 8] = [
    "INS1 person",
    "INS2 like post",
    "INS3 like comment",
    "INS4 forum",
    "INS5 membership",
    "INS6 post",
    "INS7 comment",
    "INS8 friendship",
];

/// The first `limit` events of both streams in scheduled order.
pub fn read(archive: &Path, limit: usize) -> Result<Vec<Event>> {
    let mut events = vec![];
    let mut ar = tar::Archive::new(zstd::Decoder::new(std::fs::File::open(archive)?)?);
    for entry in ar.entries()? {
        let entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
        if !name.starts_with("updateStream_") || !name.ends_with(".csv") {
            continue;
        }
        for line in BufReader::new(entry).lines() {
            let line = line?;
            let mut cells = line.split('|');
            let time = cells.next().ok_or("empty row")?.parse()?;
            let _dependency = cells.next();
            let op = cells.next().ok_or("no operation")?.parse()?;
            events.push(Event {
                time,
                op,
                fields: cells.map(str::to_owned).collect(),
            });
        }
    }
    // Stable, so events of one stream keep their order.
    events.sort_by_key(|e| e.time);
    events.truncate(limit);
    Ok(events)
}

/// A link to insert; ends are (table, LDBC id).
pub struct Link {
    pub ty: &'static str,
    pub from: (&'static str, String),
    pub to: (&'static str, String),
    pub directed: bool,
    pub props: Map<String, Json>,
}

pub struct Insert {
    pub op: u8,
    pub node: Option<(&'static NodeTable, NodeRow)>,
    pub links: Vec<Link>,
}

fn table(name: &str) -> &'static NodeTable {
    schema::node_table(name).expect("known table").1
}

fn key(table: &str, id: &str) -> String {
    format!("{table}/{id}")
}

/// Milliseconds since the epoch in the archive's `StringDateFormatter` forms.
fn civil(ms: i64) -> (i64, i64, i64, i64) {
    // Howard Hinnant's days-to-civil.
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, ms.rem_euclid(86_400_000))
}

fn date(ms: &str) -> Result<String> {
    let (y, m, d, _) = civil(ms.parse()?);
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

fn datetime(ms: &str) -> Result<String> {
    let (y, m, d, t) = civil(ms.parse()?);
    let (h, mi, s, f) = (t / 3_600_000, t / 60_000 % 60, t / 1000 % 60, t % 1000);
    Ok(format!(
        "{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{f:03}+0000"
    ))
}

/// `a;b;c` → its non-empty parts.
fn list(s: &str) -> Vec<String> {
    s.split(';')
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

fn props(pairs: &[(&str, &str)]) -> Map<String, Json> {
    pairs
        .iter()
        .filter_map(|(k, v)| Some(((*k).to_owned(), schema::value(k, v)?)))
        .collect()
}

impl Insert {
    pub fn from_event(e: &Event) -> Result<Self> {
        let f = &e.fields;
        let need = [14, 3, 3, 5, 3, 12, 11, 3][usize::from(e.op.clamp(1, 8)) - 1];
        if !(1..=8).contains(&e.op) || f.len() < need {
            return Err(format!("operation {} with {} fields", e.op, f.len()).into());
        }
        let link = |ty, from: (&'static str, &str), to: (&'static str, &str), p| Link {
            ty,
            from: (from.0, from.1.to_owned()),
            to: (to.0, to.1.to_owned()),
            directed: ty != "KNOWS",
            props: p,
        };
        let mut ins = Insert {
            op: e.op,
            node: None,
            links: vec![],
        };
        // A node from a row in its table's column order, plus its foreign-key links.
        let mut node = |t: &'static str, header: &[&str], row: Vec<String>, lists: PersonLists| {
            let spec = table(t);
            let header: Vec<String> = header.iter().map(|h| (*h).to_owned()).collect();
            let cols = spec.columns(&header)?;
            let n = spec.row(&cols, &row, &lists)?;
            for fk in spec.fks {
                let c = crate::archive::col(&header, fk.col)?;
                if row[c].is_empty() || row[c] == "-1" {
                    continue;
                }
                let (mut a, mut b) = ((t, row[0].clone()), (fk.target, row[c].clone()));
                if fk.reverse {
                    std::mem::swap(&mut a, &mut b);
                }
                ins.links.push(Link {
                    ty: fk.link,
                    from: a,
                    to: b,
                    directed: true,
                    props: Map::new(),
                });
            }
            ins.node = Some((spec, n));
            Result::Ok(())
        };
        match e.op {
            1 => {
                let mut lists = PersonLists::new();
                let id: i64 = f[0].parse()?;
                lists
                    .entry("person_speaks_language")
                    .or_default()
                    .insert(id, list(&f[9]));
                lists
                    .entry("person_email_emailaddress")
                    .or_default()
                    .insert(id, list(&f[10]));
                lists.retain(|_, m| m.values().any(|v| !v.is_empty()));
                node(
                    "person",
                    &[
                        "id",
                        "firstName",
                        "lastName",
                        "gender",
                        "birthday",
                        "creationDate",
                        "locationIP",
                        "browserUsed",
                        "place",
                    ],
                    vec![
                        f[0].clone(),
                        f[1].clone(),
                        f[2].clone(),
                        f[3].clone(),
                        date(&f[4])?,
                        datetime(&f[5])?,
                        f[6].clone(),
                        f[7].clone(),
                        f[8].clone(),
                    ],
                    lists,
                )?;
                for tag in list(&f[11]) {
                    ins.links.push(link(
                        "HAS_INTEREST",
                        ("person", &f[0]),
                        ("tag", &tag),
                        Map::new(),
                    ));
                }
                for (ty, col, cell) in [
                    ("STUDY_AT", "classYear", &f[12]),
                    ("WORK_AT", "workFrom", &f[13]),
                ] {
                    for pair in list(cell) {
                        let (org, year) = pair.split_once(',').ok_or("organisation,year")?;
                        ins.links.push(link(
                            ty,
                            ("person", &f[0]),
                            ("organisation", org),
                            props(&[(col, year)]),
                        ));
                    }
                }
            }
            2 | 3 => {
                let target = if e.op == 2 { "post" } else { "comment" };
                let p = props(&[("creationDate", &datetime(&f[2])?)]);
                ins.links
                    .push(link("LIKES", ("person", &f[0]), (target, &f[1]), p));
            }
            4 => {
                node(
                    "forum",
                    &["id", "title", "creationDate", "moderator"],
                    vec![f[0].clone(), f[1].clone(), datetime(&f[2])?, f[3].clone()],
                    PersonLists::new(),
                )?;
                for tag in list(&f[4]) {
                    ins.links
                        .push(link("HAS_TAG", ("forum", &f[0]), ("tag", &tag), Map::new()));
                }
            }
            5 => {
                let p = props(&[("joinDate", &datetime(&f[2])?)]);
                ins.links
                    .push(link("HAS_MEMBER", ("forum", &f[0]), ("person", &f[1]), p));
            }
            6 => {
                node(
                    "post",
                    &[
                        "id",
                        "imageFile",
                        "creationDate",
                        "locationIP",
                        "browserUsed",
                        "language",
                        "content",
                        "length",
                        "creator",
                        "Forum.id",
                        "place",
                    ],
                    vec![
                        f[0].clone(),
                        f[1].clone(),
                        datetime(&f[2])?,
                        f[3].clone(),
                        f[4].clone(),
                        f[5].clone(),
                        f[6].clone(),
                        f[7].clone(),
                        f[8].clone(),
                        f[9].clone(),
                        f[10].clone(),
                    ],
                    PersonLists::new(),
                )?;
                for tag in list(&f[11]) {
                    ins.links
                        .push(link("HAS_TAG", ("post", &f[0]), ("tag", &tag), Map::new()));
                }
            }
            7 => {
                node(
                    "comment",
                    &[
                        "id",
                        "creationDate",
                        "locationIP",
                        "browserUsed",
                        "content",
                        "length",
                        "creator",
                        "place",
                        "replyOfPost",
                        "replyOfComment",
                    ],
                    vec![
                        f[0].clone(),
                        datetime(&f[1])?,
                        f[2].clone(),
                        f[3].clone(),
                        f[4].clone(),
                        f[5].clone(),
                        f[6].clone(),
                        f[7].clone(),
                        f[8].clone(),
                        f[9].clone(),
                    ],
                    PersonLists::new(),
                )?;
                for tag in list(&f[10]) {
                    ins.links.push(link(
                        "HAS_TAG",
                        ("comment", &f[0]),
                        ("tag", &tag),
                        Map::new(),
                    ));
                }
            }
            _ => {
                let p = props(&[("creationDate", &datetime(&f[2])?)]);
                ins.links
                    .push(link("KNOWS", ("person", &f[0]), ("person", &f[1]), p));
            }
        }
        Ok(ins)
    }

    /// One MyGraph `write`: the new node (as `@n`) and its links, ends by key.
    pub fn write_request(&self) -> Json {
        let mut req = json!({});
        let new_key = self.node.as_ref().map(|(_, n)| n.key.clone());
        if let Some((spec, n)) = &self.node {
            let mut item = json!({ "ref": "@n", "key": n.key, "type": spec.ty, "name": n.name });
            if !n.props.is_empty() {
                item["props"] = Json::Object(n.props.clone());
            }
            req["nodes"] = json!([item]);
        }
        let end = |(t, id): &(&str, String)| {
            let k = key(t, id);
            if Some(&k) == new_key.as_ref() {
                json!("@n")
            } else {
                json!({ "key": k })
            }
        };
        let links: Vec<Json> = self
            .links
            .iter()
            .map(|l| {
                let mut item = json!({ "type": l.ty, "from": end(&l.from), "to": end(&l.to),
                                       "directed": l.directed });
                if let Some(v) = schema::valid_from(l.ty, &l.props) {
                    item["valid_from"] = v;
                }
                if !l.props.is_empty() {
                    item["props"] = Json::Object(l.props.clone());
                }
                item
            })
            .collect();
        if !links.is_empty() {
            req["links"] = json!(links);
        }
        req
    }

    /// Neo4j statements with parameters, run in one transaction; the Cypher text depends only on
    /// the operation and which properties are present, so plans are cached.
    pub fn statements(&self) -> Vec<(String, Json)> {
        let mut out = vec![];
        if let Some((spec, n)) = &self.node {
            let mut set = vec!["n.name = $name".to_owned()];
            set.extend(
                n.props
                    .keys()
                    .map(|p| format!("n.{p} = {}", conv(p, &format!("$props.{p}")))),
            );
            out.push((
                format!("MERGE (n:{} {{key: $key}}) SET {}", spec.ty, set.join(", ")),
                json!({ "key": n.key, "name": n.name, "props": plain(&n.props) }),
            ));
        }
        for l in &self.links {
            let label = |t: &str| table(t).ty;
            let arrow = if l.directed { "->" } else { "-" };
            let set: Vec<String> = l
                .props
                .keys()
                .map(|p| format!("r.{p} = {}", conv(p, &format!("$props.{p}"))))
                .collect();
            out.push((
                format!(
                    "MATCH (a:{} {{key: $a}}), (b:{} {{key: $b}}) MERGE (a)-[r:{}]{arrow}(b){}",
                    label(l.from.0),
                    label(l.to.0),
                    l.ty,
                    if set.is_empty() { String::new() } else { format!(" SET {}", set.join(", ")) }
                ),
                json!({ "a": key(l.from.0, &l.from.1), "b": key(l.to.0, &l.to.1), "props": plain(&l.props) }),
            ));
        }
        out
    }
}

/// A Cypher expression turning a parameter into the property's Neo4j type.
fn conv(prop: &str, param: &str) -> String {
    match kind(prop) {
        Kind::DateTime => format!("datetime({param})"),
        Kind::Date => format!("date({param})"),
        _ => param.to_owned(),
    }
}

/// `{"$datetime": s}` → `s`, for Neo4j parameters.
fn plain(props: &Map<String, Json>) -> Json {
    Json::Object(
        props
            .iter()
            .map(|(k, v)| {
                let v = v
                    .get("$datetime")
                    .or_else(|| v.get("$date"))
                    .cloned()
                    .unwrap_or_else(|| v.clone());
                (k.clone(), v)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_milliseconds() {
        assert_eq!(
            datetime("1347529090363").unwrap(),
            "2012-09-13T09:38:10.363+0000"
        );
        assert_eq!(date("435456000000").unwrap(), "1983-10-20");
        assert_eq!(date("-86400000").unwrap(), "1969-12-31");
    }

    #[test]
    fn a_comment_becomes_a_node_and_its_links() {
        let e = Event {
            time: 0,
            op: 7,
            fields: "1099511997932|1347529090363|1.12.242.179|Opera|roflol|6|26388279068220|1|-1|1099511997926|"
                .split('|')
                .map(str::to_owned)
                .collect(),
        };
        let ins = Insert::from_event(&e).unwrap();
        let (_, n) = ins.node.as_ref().unwrap();
        assert_eq!(n.key, "comment/1099511997932");
        assert_eq!(n.name, "Comment 1099511997932");
        assert_eq!(n.props["content"], json!("roflol"));
        let links: Vec<_> = ins.links.iter().map(|l| (l.ty, l.to.1.as_str())).collect();
        assert_eq!(
            links,
            [
                ("HAS_CREATOR", "26388279068220"),
                ("IS_LOCATED_IN", "1"),
                ("REPLY_OF", "1099511997926")
            ]
        );
        let w = ins.write_request();
        assert_eq!(w["links"][0]["from"], json!("@n"));
        assert_eq!(
            w["links"][0]["to"],
            json!({ "key": "person/26388279068220" })
        );
    }
}
