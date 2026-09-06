use std::fmt;

/// Errors returned by memory-v2 domain APIs.  Callers never need to inspect a
/// pre-redaction string to determine why an operation was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    EmptyValue(&'static str),
    InvalidRuntimeRoot,
    InvalidSubject(String),
    InvalidContent,
    InvalidSource,
    ProhibitedCategory(String),
    InvalidEmbeddingLength { expected: usize, actual: usize },
    NonFiniteEmbedding,
    CanonicalJson(String),
    InvalidOperationKind,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyValue(name) => write!(f, "{} must not be empty", name),
            Self::InvalidRuntimeRoot => write!(f, "runtime root must be an absolute path"),
            Self::InvalidSubject(value) => write!(f, "invalid memory subject: {}", value),
            Self::InvalidContent => write!(f, "content is not validated redacted text"),
            Self::InvalidSource => write!(f, "source must not be empty"),
            Self::ProhibitedCategory(category) => {
                write!(f, "fact is prohibited by category: {}", category)
            }
            Self::InvalidEmbeddingLength { expected, actual } => {
                write!(
                    f,
                    "embedding must contain {} values, got {}",
                    expected, actual
                )
            }
            Self::NonFiniteEmbedding => write!(f, "embedding contains a non-finite value"),
            Self::CanonicalJson(message) => write!(f, "cannot canonicalize JSON: {}", message),
            Self::InvalidOperationKind => write!(f, "operation kind must not be empty"),
        }
    }
}

impl std::error::Error for MemoryError {}

pub type Result<T> = std::result::Result<T, MemoryError>;
