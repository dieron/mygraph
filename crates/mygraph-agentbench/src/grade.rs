//! Grading: HotpotQA's official answer and supporting-fact metrics, and the mapping from quoted
//! evidence text back to the dataset's sentences.

use std::collections::{BTreeSet, HashMap};

/// A sentence: passage number and sentence number, both from 0.
pub type Fact = (usize, usize);

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

/// The best answer scores against any of the accepted answers (MuSiQue lists aliases).
pub fn best_answer_scores(pred: &str, golds: &[String]) -> (f64, f64, f64, f64) {
    golds
        .iter()
        .map(|g| answer_scores(pred, g))
        .fold((0.0, 0.0, 0.0, 0.0), |best, s| {
            if (s.1, s.0) > (best.1, best.0) {
                s
            } else {
                best
            }
        })
}

/// Splits a paragraph into sentences after `.`, `!` or `?` followed by a space and a capital
/// letter, digit or quote; the space starts the next sentence, so the parts join back to the text.
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..chars.len().saturating_sub(1) {
        let next = chars[i + 1];
        if chars[i] == ' '
            && matches!(chars[i - 1], '.' | '!' | '?')
            && (next.is_uppercase() || next.is_ascii_digit() || next == '"')
        {
            out.push(chars[start..i].iter().collect());
            start = i;
        }
    }
    out.push(chars[start..].iter().collect());
    out
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

/// The share of `a`'s words that occur in `b` in the same order (longest common subsequence).
fn in_order(a: &str, b: &str) -> f64 {
    let a: Vec<&str> = a.split_whitespace().collect();
    let b: Vec<&str> = b.split_whitespace().collect();
    if a.is_empty() {
        return 0.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for x in &a {
        let mut diag = 0;
        for (j, y) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
            diag = up;
        }
    }
    row[b.len()] as f64 / a.len() as f64
}

/// The passages' sentences, for mapping quoted text back to supporting facts.
pub struct Sentences {
    all: Vec<(Fact, String)>,
}

impl Sentences {
    /// From each passage's sentences, in passage order.
    pub fn new<'a>(passages: impl IntoIterator<Item = &'a [String]>) -> Self {
        let all = passages
            .into_iter()
            .enumerate()
            .flat_map(|(p, ss)| ss.iter().enumerate().map(move |(i, s)| ((p, i), words(s))))
            .filter(|(_, w)| !w.trim().is_empty())
            .collect();
        Sentences { all }
    }

    /// The sentences a quoted fragment stands for: every sentence wholly inside it, or the one(s)
    /// it is a part of (at least 3 words). A quote that is not verbatim stands for the sentences
    /// (of 5 or more words) it repeats 85 % of in order; failing those, for the sentence it is a
    /// lightly edited part of.
    pub fn matching(&self, fragment: &str) -> BTreeSet<Fact> {
        let f = words(fragment);
        let fw = f.split_whitespace().count();
        let mut out: BTreeSet<Fact> = self
            .all
            .iter()
            .filter(|(_, s)| {
                f.contains(s.as_str())
                    || (fw >= 3 && s.contains(f.as_str()))
                    || (s.split_whitespace().count() >= 5 && in_order(s, &f) >= 0.85)
            })
            .map(|(k, _)| *k)
            .collect();
        if out.is_empty() && fw >= 5 {
            out = self
                .all
                .iter()
                .filter(|(_, s)| in_order(&f, s) >= 0.85)
                .map(|(k, _)| *k)
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
        let ctx = [vec![
            "Ada Lovelace was an English mathematician.".to_owned(),
            " She worked on the Analytical Engine, a proposed computer.".to_owned(),
        ]];
        let s = Sentences::new(ctx.iter().map(Vec::as_slice));
        let both: BTreeSet<Fact> = [(0, 0), (0, 1)].into();
        let first: BTreeSet<Fact> = [(0, 0)].into();
        let second: BTreeSet<Fact> = [(0, 1)].into();
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
        // A quote that repairs the dataset's text still counts, next to verbatim sentences.
        let ctx = [vec![
            "Peter Cushing (26 May 191311 August 1994) was an English actor.".to_owned(),
            " He is mainly known for his appearances in Hammer Films.".to_owned(),
        ]];
        let s = Sentences::new(ctx.iter().map(Vec::as_slice));
        assert_eq!(
            s.matching("Peter Cushing (26 May 1913 – 11 August 1994) was an English actor. He is mainly known for his appearances in Hammer Films."),
            [(0, 0), (0, 1)].into()
        );
    }

    #[test]
    fn paragraphs_split_into_sentences_that_join_back() {
        let t = "Green is an album by Steve Hillage. It was released in 1978 by Virgin. \"Musick\"                  followed! Was it 3.5 hours long? 1999 came.";
        let ss = split_sentences(t);
        assert_eq!(ss.concat(), t);
        assert_eq!(ss.len(), 5, "{ss:?}");
        assert_eq!(ss[1], " It was released in 1978 by Virgin.");
        assert_eq!(split_sentences("One."), vec!["One."]);
    }

    #[test]
    fn aliases_count_as_answers() {
        let golds = ["Miquette Giraudy".to_owned(), "Giraudy".to_owned()];
        assert_eq!(best_answer_scores("Giraudy", &golds).0, 1.0);
        assert_eq!(best_answer_scores("Steve", &golds).1, 0.0);
    }
}
