//! How the tables of LDBC SNB Interactive v1 (`CsvMergeForeign`) map to MyGraph nodes and links
//! (docs/11-benchmarks.md §2).

use crate::archive::{Result, col};
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;

/// A table whose rows become nodes. Its foreign-key columns become links in the second pass.
pub struct NodeTable {
    pub table: &'static str,
    pub ty: &'static str,
    /// Column holding the LDBC subtype, which becomes a label (`company` → `Company`).
    pub label_col: Option<&'static str>,
    pub name: Name,
    pub fks: &'static [Fk],
}

pub enum Name {
    /// The value of one column.
    Col(&'static str),
    /// `firstName lastName`.
    Person,
    /// `<prefix> <id>`, for posts and comments, which have no name of their own.
    Id(&'static str),
}

/// A foreign-key column: a link from this row's node to the node the column names.
pub struct Fk {
    pub col: &'static str,
    pub link: &'static str,
    pub target: &'static str,
    /// The link points from the target to this row (a post's forum `CONTAINER_OF` the post).
    pub reverse: bool,
}

/// A table whose rows become links: the first two columns are the endpoints, the rest
/// properties.
pub struct EdgeTable {
    pub table: &'static str,
    pub link: &'static str,
    pub from: &'static str,
    pub to: &'static str,
    pub directed: bool,
}

/// Tables whose rows become list properties of a person.
pub const PERSON_LISTS: [(&str, &str); 2] = [
    ("person_email_emailaddress", "email"),
    ("person_speaks_language", "speaks"),
];

const fn fk(col: &'static str, link: &'static str, target: &'static str) -> Fk {
    Fk {
        col,
        link,
        target,
        reverse: false,
    }
}

pub const NODE_TABLES: [NodeTable; 8] = [
    NodeTable {
        table: "person",
        ty: "Person",
        label_col: None,
        name: Name::Person,
        fks: &[fk("place", "IS_LOCATED_IN", "place")],
    },
    NodeTable {
        table: "forum",
        ty: "Forum",
        label_col: None,
        name: Name::Col("title"),
        fks: &[fk("moderator", "HAS_MODERATOR", "person")],
    },
    NodeTable {
        table: "post",
        ty: "Post",
        label_col: None,
        name: Name::Id("Post"),
        fks: &[
            fk("creator", "HAS_CREATOR", "person"),
            Fk {
                col: "Forum.id",
                link: "CONTAINER_OF",
                target: "forum",
                reverse: true,
            },
            fk("place", "IS_LOCATED_IN", "place"),
        ],
    },
    NodeTable {
        table: "comment",
        ty: "Comment",
        label_col: None,
        name: Name::Id("Comment"),
        fks: &[
            fk("creator", "HAS_CREATOR", "person"),
            fk("place", "IS_LOCATED_IN", "place"),
            fk("replyOfPost", "REPLY_OF", "post"),
            fk("replyOfComment", "REPLY_OF", "comment"),
        ],
    },
    NodeTable {
        table: "organisation",
        ty: "Organisation",
        label_col: Some("type"),
        name: Name::Col("name"),
        fks: &[fk("place", "IS_LOCATED_IN", "place")],
    },
    NodeTable {
        table: "place",
        ty: "Place",
        label_col: Some("type"),
        name: Name::Col("name"),
        fks: &[fk("isPartOf", "IS_PART_OF", "place")],
    },
    NodeTable {
        table: "tag",
        ty: "Tag",
        label_col: None,
        name: Name::Col("name"),
        fks: &[fk("hasType", "HAS_TYPE", "tagclass")],
    },
    NodeTable {
        table: "tagclass",
        ty: "TagClass",
        label_col: None,
        name: Name::Col("name"),
        fks: &[fk("isSubclassOf", "IS_SUBCLASS_OF", "tagclass")],
    },
];

const fn edge(
    table: &'static str,
    link: &'static str,
    from: &'static str,
    to: &'static str,
) -> EdgeTable {
    EdgeTable {
        table,
        link,
        from,
        to,
        directed: true,
    }
}

pub const EDGE_TABLES: [EdgeTable; 10] = [
    EdgeTable {
        table: "person_knows_person",
        link: "KNOWS",
        from: "person",
        to: "person",
        directed: false,
    },
    edge("person_hasInterest_tag", "HAS_INTEREST", "person", "tag"),
    edge("person_likes_post", "LIKES", "person", "post"),
    edge("person_likes_comment", "LIKES", "person", "comment"),
    edge(
        "person_studyAt_organisation",
        "STUDY_AT",
        "person",
        "organisation",
    ),
    edge(
        "person_workAt_organisation",
        "WORK_AT",
        "person",
        "organisation",
    ),
    edge("forum_hasMember_person", "HAS_MEMBER", "forum", "person"),
    edge("forum_hasTag_tag", "HAS_TAG", "forum", "tag"),
    edge("post_hasTag_tag", "HAS_TAG", "post", "tag"),
    edge("comment_hasTag_tag", "HAS_TAG", "comment", "tag"),
];

pub fn node_table(table: &str) -> Option<(usize, &'static NodeTable)> {
    NODE_TABLES
        .iter()
        .enumerate()
        .find(|(_, t)| t.table == table)
}

pub fn edge_table(table: &str) -> Option<&'static EdgeTable> {
    EDGE_TABLES.iter().find(|t| t.table == table)
}

/// A table name from a file name: `person_knows_person_0_0.csv` → `person_knows_person`.
pub fn table_of(file_name: &str) -> Option<&str> {
    let stem = file_name.strip_suffix(".csv")?;
    let mut parts = stem.rsplitn(3, '_');
    let (b, a, rest) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    (numeric(a) && numeric(b)).then_some(rest)
}

/// Per person table name in [`PERSON_LISTS`]: person id → values.
pub type PersonLists = HashMap<&'static str, HashMap<i64, Vec<String>>>;

/// Where a node table's fields are in its header.
pub struct NodeColumns {
    id: usize,
    label: Option<usize>,
    name: Vec<usize>,
    /// Property columns: index and name.
    pub props: Vec<(usize, String)>,
}

/// What one node row becomes, the same for MyGraph and for the Neo4j export.
pub struct NodeRow {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub labels: Vec<String>,
    pub props: Map<String, Json>,
}

impl NodeTable {
    pub fn columns(&self, header: &[String]) -> Result<NodeColumns> {
        let id = col(header, "id")?;
        let label = self.label_col.map(|c| col(header, c)).transpose()?;
        let name = match self.name {
            Name::Col(c) => vec![col(header, c)?],
            Name::Person => vec![col(header, "firstName")?, col(header, "lastName")?],
            Name::Id(_) => vec![],
        };
        // A column that is the whole name is not repeated as a property; a person keeps
        // `firstName` and `lastName`, which LDBC queries filter on.
        let name_col = matches!(self.name, Name::Col(_)).then(|| name[0]);
        let props = header
            .iter()
            .enumerate()
            .filter(|&(i, h)| {
                i != id
                    && Some(i) != label
                    && Some(i) != name_col
                    && !self.fks.iter().any(|f| f.col == h)
            })
            .map(|(i, h)| (i, h.clone()))
            .collect();
        Ok(NodeColumns {
            id,
            label,
            name,
            props,
        })
    }

    /// The list properties this table's nodes get from [`PERSON_LISTS`].
    pub fn list_props(&self) -> &'static [(&'static str, &'static str)] {
        if self.table == "person" {
            &PERSON_LISTS
        } else {
            &[]
        }
    }

    pub fn row(&self, c: &NodeColumns, row: &[String], lists: &PersonLists) -> Result<NodeRow> {
        let id: i64 = row[c.id].parse()?;
        let name = match self.name {
            Name::Id(prefix) => format!("{prefix} {id}"),
            _ => c
                .name
                .iter()
                .map(|&i| row[i].as_str())
                .collect::<Vec<_>>()
                .join(" "),
        };
        let name = if name.trim().is_empty() {
            format!("{} {id}", self.ty)
        } else {
            name
        };
        let mut props = Map::new();
        for (i, h) in &c.props {
            if let Some(v) = value(h, &row[*i]) {
                props.insert(h.clone(), v);
            }
        }
        for (list_table, prop) in self.list_props() {
            if let Some(vs) = lists.get(list_table).and_then(|m| m.get(&id)) {
                props.insert((*prop).to_owned(), json!(vs));
            }
        }
        let labels = c
            .label
            .filter(|&l| !row[l].is_empty())
            .map(|l| label(&row[l]))
            .into_iter()
            .collect();
        Ok(NodeRow {
            id,
            key: format!("{}/{id}", self.table),
            name,
            labels,
            props,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Str,
    Int,
    Date,
    DateTime,
}

/// The kind of a column's values, by column name.
pub fn kind(col: &str) -> Kind {
    match col {
        "creationDate" | "joinDate" => Kind::DateTime,
        "birthday" => Kind::Date,
        "length" | "classYear" | "workFrom" => Kind::Int,
        _ => Kind::Str,
    }
}

/// A property value from a CSV cell, by column name; `None` leaves the property out.
pub fn value(col: &str, cell: &str) -> Option<Json> {
    if cell.is_empty() {
        return None;
    }
    Some(match kind(col) {
        Kind::DateTime => json!({ "$datetime": datetime(cell) }),
        Kind::Date => json!({ "$date": cell }),
        Kind::Int => json!(cell.parse::<i64>().ok()?),
        Kind::Str => json!(cell),
    })
}

/// LDBC writes the offset as `+0000`; MyGraph wants `Z` or `+hh:mm` ([VAL-DT-1]).
pub fn datetime(s: &str) -> String {
    match s.len().checked_sub(5).map(|i| s.split_at(i)) {
        Some((head, "+0000")) => format!("{head}Z"),
        Some((head, off)) if off.starts_with(['+', '-']) => {
            format!("{head}{}:{}", &off[..3], &off[3..])
        }
        _ => s.to_owned(),
    }
}

/// `company` → `Company`.
pub fn label(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names() {
        assert_eq!(table_of("person_0_0.csv"), Some("person"));
        assert_eq!(
            table_of("person_knows_person_12_3.csv"),
            Some("person_knows_person")
        );
        assert_eq!(table_of("updateStream.properties"), None);
        assert_eq!(table_of("person.csv"), None);
    }

    #[test]
    fn values() {
        assert_eq!(
            value("creationDate", "2011-08-17T14:26:59.961+0000"),
            Some(json!({ "$datetime": "2011-08-17T14:26:59.961Z" }))
        );
        assert_eq!(
            value("joinDate", "2011-08-17T14:26:59.961+0130"),
            Some(json!({ "$datetime": "2011-08-17T14:26:59.961+01:30" }))
        );
        assert_eq!(value("classYear", "2011"), Some(json!(2011)));
        assert_eq!(value("content", ""), None);
        assert_eq!(label("university"), "University");
    }
}
