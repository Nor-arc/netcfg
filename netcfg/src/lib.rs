//! Bidirectional network-config templates.
//!
//! A `.nct` file declares models (typed fields) and config-shaped templates. The same
//! template parses a device's running config into a value and renders a value back to
//! config. Matching is strict: a line that starts like a managed line but doesn't match is
//! an error unless the template opts out with `@ignore`.

pub mod dialect;
pub mod engine;
pub mod fmt;
pub mod lexer;
pub mod model;
pub mod schema;
pub mod template;
pub mod types;
pub mod value;

pub use dialect::Dialect;
pub use engine::{Engine, Parsed};
pub use lexer::Node;
pub use value::{Record, Value};

/// Errors are plain messages: they are meant for template authors and operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}
impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
