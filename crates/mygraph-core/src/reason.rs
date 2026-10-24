//! Reason codes (spec/README.md §3): one dotted identifier per specific failure.

use std::fmt;

/// A specific failure, identified by its spec reason code, e.g. `id.malformed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reason(pub &'static str);

impl Reason {
    pub fn code(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Reason {}
