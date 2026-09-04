//! PBN (Portable Bridge Notation) format parser and writer.
//!
//! PBN is the standard format for storing bridge hands, results, and analysis.
//! This module supports reading and writing PBN files with common tags.

mod document;
mod reader;
mod writer;

pub use document::PbnDocument;
pub use reader::{parse_tag_pair, read_pbn, read_pbn_file, TagPair};
pub use writer::{board_to_pbn, write_pbn, write_pbn_file};
