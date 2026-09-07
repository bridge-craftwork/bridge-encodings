//! Bridge file format encodings.
//!
//! This crate provides parsers and writers for common bridge file formats:
//! - **PBN** (Portable Bridge Notation) - Standard format for bridge records
//! - **LIN** - BBO (Bridge Base Online) hand record format
//! - **Oneline** - Simple format used by dealer.exe
//! - **ZRD/ZDD** - Pavlicek's binary deal and double-dummy library
//!
//! # Example
//!
//! ```
//! use bridge_encodings::pbn;
//!
//! let pbn_content = r#"
//! [Board "1"]
//! [Dealer "N"]
//! [Vulnerable "None"]
//! [Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
//! "#;
//!
//! let boards = pbn::read_pbn(pbn_content).unwrap();
//! assert_eq!(boards.len(), 1);
//! ```

mod error;
pub mod lin;
pub mod oneline;
pub mod pbn;
pub mod printall;
mod reader;
pub mod zrd;

pub use error::{ParseError, Result};
pub use reader::DealReader;

// Re-export bridge-types for convenience
pub use bridge_types::{
    Board, Card, Contract, Deal, Direction, Doubled, Hand, Rank, Strain, Suit, Vulnerability,
};
