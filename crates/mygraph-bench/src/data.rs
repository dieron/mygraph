//! Synthetic content: names, evidence text, properties and link types.
//!
//! Every value is drawn from [`Rng`] in a fixed order, so a seed and the generator version fully
//! determine the dataset. Words are built from syllables, so there is no word list to ship.

use crate::rng::Rng;
use mygraph_core::name_key::name_key;
use serde_json::{Value as Json, json};
use std::collections::HashSet;

pub const NODE_TYPES: [&str; 6] = [
    "Person",
    "Organization",
    "Place",
    "Document",
    "Concept",
    "Event",
];
/// Weights of [`NODE_TYPES`], in percent.
const TYPE_WEIGHTS: [usize; 6] = [40, 15, 10, 20, 10, 5];

const ONSETS: [&str; 20] = [
    "b", "c", "d", "f", "g", "h", "k", "l", "m", "n", "p", "r", "s", "t", "v", "w", "z", "br",
    "st", "th",
];
const VOWELS: [&str; 8] = ["a", "e", "i", "o", "u", "ai", "ea", "ou"];
const CODAS: [&str; 8] = ["", "", "", "n", "r", "s", "l", "x"];

const LANGUAGES: [&str; 6] = ["en", "de", "fr", "es", "uk", "ja"];
const SECTORS: [&str; 8] = [
    "energy",
    "finance",
    "health",
    "software",
    "transport",
    "education",
    "media",
    "agriculture",
];
const EVIDENCE_KINDS: [&str; 4] = ["text", "document", "url", "observation"];

pub struct Pools {
    pub words: Vec<String>,
    first: Vec<String>,
    last: Vec<String>,
    countries: Vec<String>,
    /// Name keys already used per node type (names and aliases), so every node item creates a
    /// new node instead of matching an earlier one ([WRITE-MATCH-NODE-1]).
    used: Vec<HashSet<String>>,
}

fn syllable(r: &mut Rng) -> String {
    format!("{}{}{}", r.pick(&ONSETS), r.pick(&VOWELS), r.pick(&CODAS))
}

fn word(r: &mut Rng, min: usize, max: usize) -> String {
    let n = min + r.below(max - min + 1);
    (0..n).map(|_| syllable(r)).collect()
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn year(r: &mut Rng) -> i64 {
    r.range(1700, 2025)
}

fn date(r: &mut Rng) -> Json {
    let y = year(r);
    match r.below(3) {
        0 => json!({ "$date": format!("{y:04}") }),
        1 => json!({ "$date": format!("{y:04}-{:02}", r.range(1, 12)) }),
        _ => json!({ "$date": format!("{y:04}-{:02}-{:02}", r.range(1, 12), r.range(1, 28)) }),
    }
}

/// A float with at most two decimals, written so it stays a float ([VAL-NUM]).
fn float(r: &mut Rng, lo: i64, hi: i64) -> Json {
    let cents = r.range(lo * 100, hi * 100);
    let sign = if cents < 0 { "-" } else { "" };
    let a = cents.abs();
    serde_json::from_str(&format!("{sign}{}.{:02}", a / 100, a % 100)).expect("float")
}

impl Pools {
    pub fn new(r: &mut Rng) -> Self {
        let words = (0..5000).map(|_| word(r, 1, 3)).collect();
        let first = (0..3000).map(|_| capital(&word(r, 2, 3))).collect();
        let last = (0..30_000).map(|_| capital(&word(r, 2, 4))).collect();
        let countries = (0..120).map(|_| capital(&word(r, 2, 3))).collect();
        Self {
            words,
            first,
            last,
            countries,
            used: vec![HashSet::new(); NODE_TYPES.len()],
        }
    }

    pub fn node_type(&self, r: &mut Rng) -> usize {
        let mut x = r.below(100);
        for (i, w) in TYPE_WEIGHTS.iter().enumerate() {
            if x < *w {
                return i;
            }
            x -= w;
        }
        0
    }

    fn text_word(&self, r: &mut Rng) -> &str {
        &self.words[r.skewed(self.words.len())]
    }

    fn phrase(&self, r: &mut Rng, n: usize) -> String {
        (0..n)
            .map(|_| self.text_word(r))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn title(&self, r: &mut Rng, n: usize) -> String {
        (0..n)
            .map(|_| capital(self.text_word(r)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn base_name(&self, r: &mut Rng, t: usize) -> String {
        match NODE_TYPES[t] {
            "Person" => format!(
                "{} {}",
                self.first[r.skewed(self.first.len())],
                self.last[r.skewed(self.last.len())]
            ),
            "Organization" => format!(
                "{} {}",
                capital(&word(r, 2, 3)),
                r.pick(&[
                    "Inc.",
                    "Institute",
                    "University",
                    "Foundation",
                    "Group",
                    "Labs"
                ])
            ),
            "Place" => capital(&word(r, 2, 4)),
            "Document" => {
                let n = 2 + r.below(5);
                self.title(r, n)
            }
            "Concept" => {
                let n = 1 + r.below(3);
                self.phrase(r, n)
            }
            _ => format!(
                "{} {} {}",
                capital(&word(r, 2, 3)),
                r.pick(&["Conference", "Summit", "Festival", "Treaty", "Expedition"]),
                year(r)
            ),
        }
    }

    /// A name not used yet for this type; collisions get a qualifier, then a number.
    fn unique(&mut self, r: &mut Rng, t: usize, base: String) -> String {
        let mut name = base.clone();
        let mut n = 1;
        while self.used[t].contains(&name_key(&name)) {
            name = if n == 1 {
                format!("{base} ({})", self.text_word(r))
            } else {
                format!("{base} ({n})")
            };
            n += 1;
        }
        self.used[t].insert(name_key(&name));
        name
    }

    fn props(&self, r: &mut Rng, t: usize) -> Json {
        let country = r.pick(&self.countries).clone();
        match NODE_TYPES[t] {
            "Person" => json!({
                "born": date(r), "country": country, "occupation": self.text_word(r),
                "score": float(r, 0, 100), "active": r.chance(0.5),
            }),
            "Organization" => json!({
                "founded": date(r), "country": country, "employees": r.range(1, 200_000),
                "sector": r.pick(&SECTORS), "public": r.chance(0.3),
            }),
            "Place" => json!({
                "country": country, "population": r.range(0, 20_000_000),
                "lat": float(r, -90, 89), "lon": float(r, -180, 179), "kind": r.pick(&["city", "town", "river", "region"]),
            }),
            "Document" => json!({
                "published": date(r), "pages": r.range(1, 900), "language": r.pick(&LANGUAGES),
                "cited_by": r.skewed(5000), "format": r.pick(&["pdf", "html", "book", "article"]),
            }),
            "Concept" => json!({
                "field": self.text_word(r), "introduced": date(r), "score": float(r, 0, 1),
                "popularity": r.skewed(100_000), "tags": [self.text_word(r), self.text_word(r)],
            }),
            _ => json!({
                "date": date(r), "country": country, "attendees": r.range(10, 100_000),
                "kind": r.pick(&["meeting", "war", "launch", "election"]), "virtual": r.chance(0.2),
            }),
        }
    }

    /// One node item citing the batch evidence `ev_ref`, plus that evidence item.
    pub fn node_with_evidence(&mut self, r: &mut Rng, t: usize, ev_ref: &str) -> (Json, Json) {
        let base = self.base_name(r, t);
        let name = self.unique(r, t, base);
        let mut node = json!({
            "type": NODE_TYPES[t], "name": name, "props": self.props(r, t), "cite": [ev_ref],
        });
        if r.chance(0.15) {
            let alias = match NODE_TYPES[t] {
                "Person" => name
                    .split_once(' ')
                    .map(|(f, l)| format!("{}. {l}", &f[..1]))
                    .unwrap_or_default(),
                _ => name
                    .split(' ')
                    .filter_map(|w| w.chars().next())
                    .collect::<String>()
                    .to_uppercase(),
            };
            if alias.chars().count() > 1 && !self.used[t].contains(&name_key(&alias)) {
                self.used[t].insert(name_key(&alias));
                node["aliases"] = json!([alias]);
            }
        }
        if r.chance(0.3) {
            node["summary"] = format!("{} {}.", name, self.phrase(r, 8)).into();
        }
        let kind = *r.pick(&EVIDENCE_KINDS);
        let sentences = 2 + r.below(5);
        let mut content = format!("{}. ", self.title(r, 4));
        for i in 0..sentences {
            let n = 8 + r.below(10);
            let s = self.phrase(r, n);
            if i == 0 {
                content.push_str(&format!("{name} {s}. "));
            } else {
                content.push_str(&format!("{}. ", capital(&s)));
            }
        }
        let mut ev = json!({
            "ref": ev_ref, "kind": kind, "content": content.trim_end(),
            "title": self.title(r, 3), "source_date": date(r),
        });
        if kind == "url" {
            ev["uri"] = format!(
                "https://example.org/{}/{}",
                self.text_word(r),
                r.next_u64() % 1_000_000
            )
            .into();
        }
        if r.chance(0.4) {
            ev["author"] = format!(
                "{} {}",
                r.pick(&self.first).clone(),
                r.pick(&self.last).clone()
            )
            .into();
        }
        (node, ev)
    }
}

/// Link type and direction for an ordered pair of node types; `swap` reverses the endpoints.
pub fn link_type(a: usize, b: usize) -> (&'static str, bool, bool) {
    let name = |i: usize| NODE_TYPES[i];
    let rule = |x: &str, y: &str| -> Option<(&'static str, bool)> {
        Some(match (x, y) {
            ("Person", "Person") => ("KNOWS", false),
            ("Person", "Organization") => ("WORKS_AT", true),
            ("Person", "Place") => ("LIVES_IN", true),
            ("Person", "Document") => ("AUTHORED", true),
            ("Person", "Concept") => ("STUDIES", true),
            ("Person", "Event") => ("ATTENDED", true),
            ("Organization", "Organization") => ("PARTNERS_WITH", false),
            ("Organization", "Place") => ("LOCATED_IN", true),
            ("Event", "Place") => ("HELD_IN", true),
            ("Place", "Place") => ("NEAR", false),
            ("Concept", "Concept") => ("RELATED_TO", false),
            ("Document", _) => ("MENTIONS", true),
            _ => return None,
        })
    };
    if let Some((t, d)) = rule(name(a), name(b)) {
        (t, d, false)
    } else if let Some((t, d)) = rule(name(b), name(a)) {
        (t, d, true)
    } else {
        ("RELATED_TO", false, false)
    }
}

/// Optional link fields: some links carry properties or a validity start.
pub fn link_extras(r: &mut Rng, link: &mut Json) {
    if r.chance(0.3) {
        link["props"] = json!({ "weight": float(r, 0, 1), "source_count": r.range(1, 20) });
    }
    if r.chance(0.1) {
        link["valid_from"] = json!({ "$date": format!("{:04}", year(r)) });
    }
}
