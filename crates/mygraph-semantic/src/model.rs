//! The embedding model: multilingual-e5-small as ONNX, run by ONNX Runtime through fastembed,
//! loaded from local files only (no download at run time).

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use mygraph_core::store::{Closeness, StoreFailure};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The model's name: the folder it is read from, and part of every index built with it, so an
/// index built by another model is rebuilt.
pub const MODEL: &str = "multilingual-e5-small";
/// The files of a model folder.
pub const MODEL_FILES: [&str; 5] = [
    "model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];
/// The least similarity of a search hit unless the server sets another
/// (`search.semantic_threshold`). e5 similarities are compressed: unrelated texts score about
/// 0.74–0.78, answers to a question about 0.80–0.87. At 0.80, half the hits found by meaning only
/// in 160 LongMemEval searches go away, and the messages holding answers that go with them were
/// found by words too (docs/11-benchmarks.md §4).
pub const DEFAULT_THRESHOLD: f32 = 0.80;
/// Tokens the model reads of one passage (its limit is 512); passages are cut shorter than this
/// ([`mygraph_core::store::passages`]), so this only guards against very long words.
const MAX_TOKENS: usize = 512;

/// `<data_dir>/models/multilingual-e5-small/`, where the model is looked for when no folder is
/// configured.
pub fn default_model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODEL)
}

/// What a text is: e5 models embed questions and the passages that answer them differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Query,
    Passage,
}

/// Turns texts into unit vectors whose dot product measures closeness in meaning. The ONNX model
/// is one; tests use a fake.
pub trait Embed: Send + Sync {
    /// Recorded with an index: a different value rebuilds it.
    fn id(&self) -> String;
    fn dims(&self) -> usize;
    fn embed(&self, texts: &[String], role: Role) -> Result<Vec<Vec<f32>>, StoreFailure>;
    /// The least similarity (dot product) of a hit that is `closeness` close to a query.
    fn floor(&self, closeness: Closeness) -> f32;
}

/// The ONNX model. Runs are serialized (one session); callers keep batches small so a query
/// never waits long behind indexing.
pub struct Model {
    session: Mutex<TextEmbedding>,
    /// [`Closeness::Related`]: the least similarity of a search hit.
    related: f32,
}

impl Model {
    /// Loads the model from a folder holding [`MODEL_FILES`].
    pub fn load(dir: &Path) -> Result<Self, StoreFailure> {
        let read = |name: &str| {
            std::fs::read(dir.join(name))
                .map_err(|e| format!("embedding model {}: {e}", dir.join(name).display()))
        };
        let model = UserDefinedEmbeddingModel::new(
            read("model.onnx")?,
            TokenizerFiles {
                tokenizer_file: read("tokenizer.json")?,
                config_file: read("config.json")?,
                special_tokens_map_file: read("special_tokens_map.json")?,
                tokenizer_config_file: read("tokenizer_config.json")?,
            },
        )
        .with_pooling(Pooling::Mean);
        let options = InitOptionsUserDefined::new().with_max_length(MAX_TOKENS);
        let session = TextEmbedding::try_new_from_user_defined(model, options)?;
        Ok(Self {
            session: Mutex::new(session),
            related: DEFAULT_THRESHOLD,
        })
    }

    /// The least similarity of a search hit ([`Closeness::Related`]), instead of
    /// [`DEFAULT_THRESHOLD`].
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.related = threshold;
        self
    }
}

impl Embed for Model {
    fn id(&self) -> String {
        MODEL.to_owned()
    }

    fn dims(&self) -> usize {
        384
    }

    /// Search hits: [`DEFAULT_THRESHOLD`] or the server's setting. Close: a name and its
    /// spelled-out form ("IBM", "International Business Machines") score about 0.86 (measured on
    /// LongMemEval histories, docs/11-benchmarks.md §4).
    fn floor(&self, closeness: Closeness) -> f32 {
        match closeness {
            Closeness::Related => self.related,
            Closeness::Close => 0.85,
        }
    }

    fn embed(&self, texts: &[String], role: Role) -> Result<Vec<Vec<f32>>, StoreFailure> {
        let prefix = match role {
            Role::Query => "query: ",
            Role::Passage => "passage: ",
        };
        let input: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = session.embed(&input, Some(input.len().max(1)))?;
        for v in &mut out {
            normalize(v);
        }
        Ok(out)
    }
}

/// Scales a vector to length 1 (a zero vector stays zero).
pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}
