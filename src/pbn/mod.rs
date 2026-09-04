//! PBN (Portable Bridge Notation) format parser and writer.
//!
//! PBN is the standard format for storing bridge hands, results, and analysis.
//! This module supports reading and writing PBN files with common tags.

mod document;
mod reader;
mod writer;

pub mod dd;
pub use dd::{
    dd_table_from_pbn, dd_table_to_pbn, is_optimum_result_row, optimum_result_table_from_rows,
    optimum_result_table_rows, OPTIMUM_RESULT_TABLE_HEADER,
};
pub use document::{prevailing_newline, split_lines, PbnDocument};
pub use reader::{parse_tag_line, parse_tag_pair, read_pbn, read_pbn_file, TagPair};
pub use writer::{board_to_pbn, write_pbn, write_pbn_file};
