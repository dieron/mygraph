//! Grading: HotpotQA's official answer and supporting-fact metrics, and the mapping from quoted
//! evidence text back to the dataset's sentences.

use std::collections::{BTreeSet, HashMap};

/// A supporting fact: passage title and sentence number.
pub type Fact = (String, usize);

/// HotpotQA's `normalize_answer`: lower case, no punctuation, no articles, single spaces.
pub fn normalize_answer(s: &str) -> String {
    let lower = s.to_lowercase();
    let no_punct: String = lower
        .chars()
        .filter(|c| !c.is_ascii_punctuation())
        .collect();
    no_punct
        .split_whitespace()
        .filter(|w| !matches!(*w, "a" | "an" | "the"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Answer exact match and (F1, precision, recall), as HotpotQA's `update_answer`.
pub fn answer_scores(pred: &str, gold: &str) -> (f64, f64, f64, f64) {
    let (p, g) = (normalize_answer(pred), normalize_answer(gold));
    let em = f64::from(p == g);
    let special = ["yes", "no", "noanswer"];
    if (special.contains(&p.as_str()) || special.contains(&g.as_str())) && p != g {
        return (em, 0.0, 0.0, 0.0);
    }
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for t in g.split_whitespace() {
        *counts.entry(t).or_default() += 1;
    }
    let mut same = 0;
    for t in p.split_whitespace() {
        if let Some(c) = counts.get_mut(t).filter(|c| **c > 0) {
            *c -= 1;
            same += 1;
        }
    }
    if same == 0 {
        return (em, 0.0, 0.0, 0.0);
    }
    let prec = same as f64 / p.split_whitespace().count() as f64;
    let rec = same as f64 / g.split_whitespace().count() as f64;
    (em, 2.0 * prec * rec / (prec + rec), prec, rec)
}

/// Set exact match and (F1, precision, recall), as HotpotQA's `update_sp`.
pub fn set_scores<T: Ord>(pred: &BTreeSet<T>, gold: &BTreeSet<T>) -> (f64, f64, f64, f64) {
    let tp = pred.intersection(gold).count() as f64;
    let fp = pred.len() as f64 - tp;
    let fne = gold.len() as f64 - tp;
    let prec = if tp + fp > 0.0 { tp / (tp + fp) } else { 0.0 };
    let rec = if tp + fne > 0.0 { tp / (tp + fne) } else { 0.0 };
    let f1 = if prec + rec > 0.0 {
        2.0 * prec * rec / (prec + rec)
    } else {
        0.0
    };
    (f64::from(fp + fne == 0.0), f1, prec, rec)
}

/// Words of `s`, lower case, letters and digits only, space-separated with a space at each end.
fn words(s: &str) -> String {
    let mut out = String::from(" ");
    for w in s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        out.push_str(w);
        out.push(' ');
    }
    out
}

/// The share of `a`'s words (with repeats) that also occur in `b`.
fn covered(a: &str, b: &str) -> f64 {
    let mut pool: HashMap<&str, i64> = HashMap::new();
    for w in b.split_whitespace() {
        *pool.entry(w).or_default() += 1;
    }
    let (mut n, mut hit) = (0, 0);
    for w in a.split_whitespace() {
        n += 1;
        if let Some(c) = pool.get_mut(w).filter(|c| **c > 0) {
            *c -= 1;
            hit += 1;
        }
    }
    if n == 0 { 0.0 } else { hit as f64 / n as f64 }
}

/// The passages' sentences, for mapping quoted text back to supporting facts.
pub struct Sentences {
    all: Vec<(Fact, String)>,
}

impl Sentences {
    pub fn new(context: &[(String, Vec<String>)]) -> Self {
        let all = context
            .iter()
            .flat_map(|(t, ss)| {
                ss.iter()
                    .enumerate()
                    .map(move |(i, s)| ((t.clone(), i), words(s)))
            })
            .filter(|(_, w)| !w.trim().is_empty())
            .collect();
        Sentences { all }
    }

    /// The sentences a quoted fragment stands for: every sentence wholly inside it, or the one(s)
    /// it is a part of (at least 3 words), or, when the quote is not verbatim, the sentences whose
    /// words it mostly repeats (80 % of a sentence of 5 or more words).
    pub fn matching(&self, fragment: &str) -> BTreeSet<Fact> {
        let f = words(fragment);
        let fw = f.split_whitespace().count();
        let mut out: BTreeSet<Fact> = self
            .all
            .iter()
            .filter(|(_, s)| f.contains(s.as_str()) || (fw >= 3 && s.contains(f.as_str())))
            .map(|(k, _)| k.clone())
            .collect();
        if out.is_empty() {
            out = self
                .all
                .iter()
                .filter(|(_, s)| {
                    let sw = s.split_whitespace().count();
                    (sw >= 5 && covered(s, &f) >= 0.8) || (fw >= 5 && covered(&f, s) >= 0.9)
                })
                .map(|(k, _)| k.clone())
                .collect();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_follow_the_official_normalization() {
        assert_eq!(normalize_answer("The  Beatles!"), "beatles");
        assert_eq!(answer_scores("the Beatles", "Beatles").0, 1.0);
        let (em, f1, p, r) = answer_scores("Paul McCartney", "McCartney");
        assert_eq!((em, p, r), (0.0, 0.5, 1.0));
        assert!((f1 - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(answer_scores("yes", "no").1, 0.0);
        assert_eq!(answer_scores("no", "no").1, 1.0);
    }

    #[test]
    fn sets_score_like_update_sp() {
        let g: BTreeSet<i32> = [1, 2].into();
        assert_eq!(set_scores(&[1, 2].into(), &g), (1.0, 1.0, 1.0, 1.0));
        let (em, f1, p, r) = set_scores(&[1, 3].into(), &g);
        assert_eq!((em, f1, p, r), (0.0, 0.5, 0.5, 0.5));
        assert_eq!(set_scores(&BTreeSet::new(), &g), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn quotes_map_to_sentences() {
        let ctx = vec![(
            "Ada".to_owned(),
            vec![
                "Ada Lovelace was an English mathematician.".to_owned(),
                " She worked on the Analytical Engine, a proposed computer.".to_owned(),
            ],
        )];
        let s = Sentences::new(&ctx);
        let both: BTreeSet<Fact> = [("Ada".into(), 0), ("Ada".into(), 1)].into();
        let first: BTreeSet<Fact> = [("Ada".into(), 0)].into();
        let second: BTreeSet<Fact> = [("Ada".into(), 1)].into();
        // The whole passage, one sentence, part of one, a lightly edited quote.
        assert_eq!(
            s.matching("Ada Lovelace was an English mathematician. She worked on the Analytical Engine, a proposed computer."),
            both
        );
        assert_eq!(
            s.matching("ada lovelace was an English mathematician"),
            first
        );
        assert_eq!(s.matching("worked on the Analytical Engine"), second);
        assert_eq!(
            s.matching("She worked on the Analytical Engine, which was a proposed computer"),
            second
        );
        assert!(s.matching("an English").is_empty());
    }
}
