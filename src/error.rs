//! Error types for bridge file format parsing.

use thiserror::Error;

/// Errors that can occur when parsing bridge file formats
#[derive(Error, Debug)]
pub enum ParseError {
    #[error("PBN parse error: {0}")]
    Pbn(String),

    #[error("LIN parse error: {0}")]
    Lin(String),

    #[error("Oneline parse error: {0}")]
    Oneline(String),

    #[error("ZRD parse error: {0}")]
    Zrd(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for bridge parsing operations
pub type Result<T> = std::result::Result<T, ParseError>;
