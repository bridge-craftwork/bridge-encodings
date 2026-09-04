//! Edit-in-place PBN documents.
//!
//! [`read_pbn`](super::read_pbn) and [`write_pbn`](super::write_pbn) are the
//! right tools for reading a file into a typed model, or for generating a new
//! file from one. Neither is right for the third case: *touching up* a file a
//! human authored — changing or adding a tag on some boards and leaving every
//! other byte exactly as the author wrote it.
//!
//! Parsing to a model and re-emitting cannot do that. It normalises whitespace,
//! tag order and layout, and drops the `%` directives where Bridge Composer
//! keeps fonts, page setup and colours. A caller that meant only to annotate a
//! file would reformat the author's work.
//!
//! [`PbnDocument`] holds the original text and an index into it. An edit
//! splices lines into one board's record; every other block is emitted from the
//! original bytes, so an unedited document round-trips byte-for-byte by
//! construction — CRLF, mixed endings, a missing final newline and all. That is
//! what makes "re-runs are no-ops" possible: a caller can annotate a tree
//! repeatedly without churning mtimes.

use std::fmt;
use std::ops::Range;
use std::path::Path;

use bridge_types::Board;

use super::reader::{parse_tag_pair, read_pbn};
use crate::error::{ParseError, Result};

/// The PBN mandatory tag set, in the order the standard lists it. New tags are
/// placed relative to these, so an inserted tag lands where a reader expects it
/// rather than at the end of the record.
const MANDATORY_TAGS: [&str; 15] = [
    "Event",
    "Site",
    "Date",
    "Board",
    "West",
    "North",
    "East",
    "South",
    "Dealer",
    "Vulnerable",
    "Deal",
    "Scoring",
    "Declarer",
    "Contract",
    "Result",
];

/// One record of the file: a run of lines up to and including the blank line(s)
/// terminating it. Blocks tile the whole file, so concatenating them in order
/// reproduces it exactly.
struct Block {
    /// Byte range in the document's original text.
    range: Range<usize>,
    /// Replacement text, present only once an edit made this block differ from
    /// the original bytes.
    edited: Option<String>,
}

/// A tag and the data lines belonging to it, as line indices within a block.
///
/// A tag such as `OptimumResultTable` or `Auction` is a header followed by its
/// rows; removing or replacing one means taking the whole span, not one line.
struct TagSpan {
    name: String,
    start: usize,
    /// Exclusive; `start + 1` for an ordinary single-line tag.
    end: usize,
}

/// A PBN file kept as written, with an index into it.
///
/// See the [module documentation](self) for when to reach for this rather than
/// [`read_pbn`](super::read_pbn) / [`write_pbn`](super::write_pbn).
///
/// ```
/// use bridge_encodings::pbn::PbnDocument;
///
/// let src = "% Bridge Composer 5.9\r\n\r\n[Board \"1\"]\r\n[Result \"9\"]\r\n";
/// let mut doc = PbnDocument::parse(src)?;
/// assert!(!doc.is_modified());
/// assert_eq!(doc.to_pbn(), src); // untouched: byte-for-byte
///
/// doc.set_tag(0, "DoubleDummyTricks", "AAAA")?;
/// assert!(doc.is_modified());
/// // Inserted after the mandatory tags, with the file's own line ending.
/// assert!(doc.to_pbn().contains("[Result \"9\"]\r\n[DoubleDummyTricks \"AAAA\"]\r\n"));
/// # Ok::<(), bridge_encodings::ParseError>(())
/// ```
pub struct PbnDocument {
    /// The file as given. Never mutated.
    text: String,
    /// Line ending for inserted lines when no neighbouring line offers one.
    newline: &'static str,
    blocks: Vec<Block>,
    boards: Vec<Board>,
    /// `boards[i]` was parsed from `blocks[board_blocks[i]]`.
    board_blocks: Vec<usize>,
}

impl PbnDocument {
    /// Parse PBN content, keeping the original text for editing.
    pub fn parse(content: &str) -> Result<Self> {
        let text = content.to_string();
        let ranges = split_blocks(&text);

        let mut blocks = Vec::with_capacity(ranges.len());
        let mut boards = Vec::new();
        let mut board_blocks = Vec::new();
        for (index, range) in ranges.into_iter().enumerate() {
            for board in read_pbn(&text[range.clone()])? {
                boards.push(board);
                board_blocks.push(index);
            }
            blocks.push(Block {
                range,
                edited: None,
            });
        }

        Ok(Self {
            newline: prevailing_newline(&text),
            text,
            blocks,
            boards,
            board_blocks,
        })
    }

    /// Parse a PBN file, keeping the original text for editing.
    pub fn parse_file(path: &Path) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// The boards, as [`read_pbn`](super::read_pbn) would return them.
    ///
    /// Board indices address the same records that [`set_tag`](Self::set_tag)
    /// and its companions take. Blocks carrying no tags — a leading `%` header,
    /// a run of blank lines — are not boards and are never addressable; they
    /// pass through untouched.
    pub fn boards(&self) -> &[Board] {
        &self.boards
    }

    /// The value of one board's tag, exactly as written, or `None` if the board
    /// does not carry it.
    ///
    /// For a tag with data rows this is the header's quoted value; the rows are
    /// available through [`tag_rows`](Self::tag_rows).
    pub fn tag(&self, board: usize, name: &str) -> Option<&str> {
        let lines = self.block_lines(board)?;
        let spans = tag_spans(&lines);
        let span = spans.iter().find(|s| s.name == name)?;
        tag_value(lines[span.start].0)
    }

    /// The data rows following one board's tag, exactly as written.
    ///
    /// Empty for an ordinary single-line tag, and for a tag the board does not
    /// carry.
    pub fn tag_rows(&self, board: usize, name: &str) -> Vec<&str> {
        let Some(lines) = self.block_lines(board) else {
            return Vec::new();
        };
        let spans = tag_spans(&lines);
        let Some(span) = spans.iter().find(|s| s.name == name) else {
            return Vec::new();
        };
        lines[span.start + 1..span.end]
            .iter()
            .map(|(content, _)| *content)
            .collect()
    }

    /// Set or insert a single-line tag on one board, leaving every other byte
    /// of the file alone.
    ///
    /// An existing tag of that name is replaced in place, along with any data
    /// rows it carried. A new tag is inserted where the standard's tag order
    /// puts it: among the mandatory tags in their listed order, else
    /// alphabetically among the supplemental tags, and always ahead of the
    /// `Auction` and `Play` sections.
    ///
    /// Setting a tag to the value it already holds is not a modification; see
    /// [`is_modified`](Self::is_modified).
    ///
    /// # Errors
    ///
    /// If `board` is out of range, or `name` or `value` could not be written
    /// back as a single well-formed tag line.
    pub fn set_tag(&mut self, board: usize, name: &str, value: &str) -> Result<()> {
        self.set_section(board, name, value, &[])
    }

    /// Set or insert a tag together with the data rows belonging to it.
    ///
    /// This is the multi-line form of [`set_tag`](Self::set_tag), for tags such
    /// as `OptimumResultTable` that are a header followed by their own rows.
    /// Replacing one replaces the whole span, so the previous rows do not
    /// survive under the new header. Rows are written with the line ending the
    /// surrounding file uses.
    ///
    /// # Errors
    ///
    /// As [`set_tag`](Self::set_tag); additionally if a row is not a single
    /// line.
    pub fn set_section(
        &mut self,
        board: usize,
        name: &str,
        value: &str,
        rows: &[&str],
    ) -> Result<()> {
        validate_tag_name(name)?;
        validate_value(value)?;
        for row in rows {
            validate_row(row)?;
        }
        let mut replacement = Vec::with_capacity(rows.len() + 1);
        replacement.push(format!("[{name} \"{value}\"]"));
        replacement.extend(rows.iter().map(|row| (*row).to_string()));
        self.edit(board, name, Some(replacement))
    }

    /// Remove a tag from one board, along with any data rows belonging to it.
    ///
    /// Removing a tag the board does not carry is not a modification.
    ///
    /// # Errors
    ///
    /// If `board` is out of range.
    pub fn remove_tag(&mut self, board: usize, name: &str) -> Result<()> {
        self.edit(board, name, None)
    }

    /// Whether an edit has actually changed the file.
    ///
    /// Setting a tag to the value it already holds leaves this `false`, so a
    /// caller can skip the write and leave the file's mtime alone.
    pub fn is_modified(&self) -> bool {
        self.blocks.iter().any(|block| block.edited.is_some())
    }

    /// The file, with only the edits applied.
    pub fn to_pbn(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        for block in &self.blocks {
            match &block.edited {
                Some(edited) => out.push_str(edited),
                None => out.push_str(&self.text[block.range.clone()]),
            }
        }
        out
    }

    /// Write the file, with only the edits applied.
    pub fn write_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_pbn())
    }

    /// The current text of the block holding `board`, split into lines.
    fn block_lines(&self, board: usize) -> Option<Vec<(&str, &str)>> {
        let block = self.blocks.get(*self.board_blocks.get(board)?)?;
        let text = match &block.edited {
            Some(edited) => edited.as_str(),
            None => &self.text[block.range.clone()],
        };
        Some(split_lines(text))
    }

    /// Replace, remove or insert one tag span in the block holding `board`,
    /// then re-render that block. A render identical to the original bytes
    /// clears the edit, so a no-op edit leaves the document unmodified.
    fn edit(&mut self, board: usize, name: &str, replacement: Option<Vec<String>>) -> Result<()> {
        let block_index = *self.board_blocks.get(board).ok_or_else(|| {
            ParseError::Pbn(format!(
                "board index {board} out of range ({} board(s) in document)",
                self.boards.len()
            ))
        })?;

        let original = &self.text[self.blocks[block_index].range.clone()];
        let current = match &self.blocks[block_index].edited {
            Some(edited) => edited.as_str(),
            None => original,
        };
        let mut lines: Vec<(String, String)> = split_lines(current)
            .into_iter()
            .map(|(content, term)| (content.to_string(), term.to_string()))
            .collect();

        let spans = tag_spans(&lines);
        let existing = spans.iter().find(|span| span.name == name);

        match (existing, replacement) {
            (Some(span), Some(new)) => {
                let (start, end) = (span.start, span.end);
                // Prefer the ending the replaced header already used, so a file
                // with mixed endings keeps this record's.
                let newline = match lines[start].1.as_str() {
                    "" => pick_newline(&lines, start, self.newline).to_string(),
                    own => own.to_string(),
                };
                remove_lines(&mut lines, start, end);
                insert_lines(&mut lines, start, new, &newline);
            }
            (Some(span), None) => {
                let (start, end) = (span.start, span.end);
                remove_lines(&mut lines, start, end);
            }
            (None, Some(new)) => {
                let at = insertion_point(&spans, &lines, name);
                let newline = pick_newline(&lines, at, self.newline).to_string();
                insert_lines(&mut lines, at, new, &newline);
            }
            // Removing a tag that is not there changes nothing.
            (None, None) => return Ok(()),
        }

        let mut rendered = String::with_capacity(current.len() + 64);
        for (content, term) in &lines {
            rendered.push_str(content);
            rendered.push_str(term);
        }
        self.blocks[block_index].edited = (rendered != original).then_some(rendered);
        Ok(())
    }
}

impl fmt::Display for PbnDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_pbn())
    }
}

/// Split `text` into `(content, terminator)` pairs, each line's ending kept
/// exactly as written. The final pair's terminator is empty when the text ends
/// without a newline, so rejoining is lossless for LF, CRLF and mixed files.
fn split_lines(text: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] != b'\n' {
            continue;
        }
        let term = if i > start && bytes[i - 1] == b'\r' {
            i - 1
        } else {
            i
        };
        lines.push((&text[start..term], &text[term..=i]));
        start = i + 1;
    }
    if start < text.len() {
        lines.push((&text[start..], ""));
    }
    lines
}

/// The line ending most of `text` uses, for inserted lines with no neighbour to
/// copy from. An empty or single-line file gets `\n`.
fn prevailing_newline(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf > lf {
        "\r\n"
    } else {
        "\n"
    }
}

/// Track `{...}` commentary across a line. Braces do not nest in PBN.
fn update_braces(line: &str, mut open: bool) -> bool {
    for ch in line.chars() {
        match ch {
            '{' => open = true,
            '}' => open = false,
            _ => {}
        }
    }
    open
}

/// Byte ranges of the file's blocks, in order and tiling it completely.
///
/// A blank line terminates a block, except inside `{...}` commentary, where a
/// blank line is just part of the comment. The blank run belongs to the block it
/// closes, so the ranges leave no gaps and concatenate back to the original.
fn split_blocks(text: &str) -> Vec<Range<usize>> {
    let lines = split_lines(text);
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    let mut in_braces = false;
    let mut i = 0;

    while i < lines.len() {
        let (content, term) = lines[i];
        if content.trim().is_empty() && !in_braces {
            let mut end = offset;
            while i < lines.len() && lines[i].0.trim().is_empty() {
                end += lines[i].0.len() + lines[i].1.len();
                i += 1;
            }
            ranges.push(start..end);
            start = end;
            offset = end;
            continue;
        }
        in_braces = update_braces(content, in_braces);
        offset += content.len() + term.len();
        i += 1;
    }

    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

/// Index every tag in a block, each with the data rows belonging to it.
///
/// A data row is any line that is not another tag, a comment, a directive or
/// blank — the rule the standard gives for the lines under a table tag, which
/// works for `OptimumResultTable` and `Auction` alike without naming either.
fn tag_spans<S: AsRef<str>>(lines: &[(S, S)]) -> Vec<TagSpan> {
    let mut spans = Vec::new();
    let mut in_braces = false;
    let mut i = 0;

    while i < lines.len() {
        let content = lines[i].0.as_ref();
        if in_braces {
            in_braces = update_braces(content, in_braces);
            i += 1;
            continue;
        }
        let Some(tag) = parse_tag_pair(content.trim()) else {
            in_braces = update_braces(content, in_braces);
            i += 1;
            continue;
        };
        let start = i;
        in_braces = update_braces(content, in_braces);
        i += 1;
        while i < lines.len() && !in_braces {
            let row = lines[i].0.as_ref();
            let trimmed = row.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('[')
                || trimmed.starts_with('{')
                || trimmed.starts_with('%')
                || trimmed.starts_with(';')
            {
                break;
            }
            in_braces = update_braces(row, in_braces);
            i += 1;
        }
        spans.push(TagSpan {
            name: tag.name,
            start,
            end: i,
        });
    }
    spans
}

/// Sort key deciding where a new tag lands: the mandatory tags first in the
/// order the standard lists them, then supplemental tags alphabetically, then
/// the game-record sections, which must follow every tag they describe.
fn tag_rank(name: &str) -> (u8, usize, &str) {
    if let Some(position) = MANDATORY_TAGS.iter().position(|tag| *tag == name) {
        return (0, position, "");
    }
    match name {
        // `Note` annotates the auction it follows, so it sorts with the
        // sections rather than among the supplemental tags.
        "Auction" | "Play" | "Note" => (2, 0, name),
        _ => (1, 0, name),
    }
}

/// The line index a new `name` tag should be inserted at.
fn insertion_point<S: AsRef<str>>(spans: &[TagSpan], lines: &[(S, S)], name: &str) -> usize {
    let rank = tag_rank(name);
    if let Some(span) = spans.iter().find(|span| tag_rank(&span.name) > rank) {
        return span.start;
    }
    if let Some(last) = spans.last() {
        // After every tag, but ahead of any trailing commentary or blank lines.
        return last.end;
    }
    // A block with no tags at all: land after any leading directives, so a `%`
    // header keeps its place at the top.
    lines
        .iter()
        .position(|(content, _)| {
            let trimmed = content.as_ref().trim();
            !trimmed.is_empty() && !trimmed.starts_with('%') && !trimmed.starts_with(';')
        })
        .unwrap_or(lines.len())
}

/// The line ending to give lines inserted at `at`: the one its new neighbours
/// use, falling back to the file's prevailing ending.
fn pick_newline<'a>(lines: &'a [(String, String)], at: usize, fallback: &'a str) -> &'a str {
    let neighbours = [at.checked_sub(1), Some(at)];
    for index in neighbours.into_iter().flatten() {
        if let Some((_, term)) = lines.get(index) {
            if !term.is_empty() {
                return term;
            }
        }
    }
    fallback
}

/// Drop lines `start..end`, keeping a file that ended without a newline ending
/// without one.
fn remove_lines(lines: &mut Vec<(String, String)>, start: usize, end: usize) {
    let dropped_unterminated_tail =
        end == lines.len() && lines.last().is_some_and(|(_, term)| term.is_empty());
    lines.drain(start..end);
    if dropped_unterminated_tail {
        if let Some((_, term)) = lines.last_mut() {
            term.clear();
        }
    }
}

/// Splice `new` in at `at`, each line ended with `newline`.
fn insert_lines(lines: &mut Vec<(String, String)>, at: usize, new: Vec<String>, newline: &str) {
    if new.is_empty() {
        return;
    }
    // Appending past a final line that has no ending: give that line one, and
    // let the last inserted line inherit the missing trailing newline, so a file
    // written without one still is.
    let mut trailing = newline.to_string();
    if at == lines.len() {
        if let Some((_, term)) = lines.last_mut() {
            if term.is_empty() {
                term.push_str(newline);
                trailing = String::new();
            }
        }
    }
    let last = new.len() - 1;
    let spliced: Vec<(String, String)> = new
        .into_iter()
        .enumerate()
        .map(|(index, content)| {
            let term = if index == last {
                trailing.clone()
            } else {
                newline.to_string()
            };
            (content, term)
        })
        .collect();
    lines.splice(at..at, spliced);
}

/// The quoted value of a tag line, as written.
fn tag_value(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let open = trimmed.find('"')?;
    let close = trimmed.rfind('"')?;
    (close > open).then(|| &trimmed[open + 1..close])
}

/// Reject a tag name that could not be written back and read as itself.
fn validate_tag_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '[' | ']' | '"'))
    {
        return Err(ParseError::Pbn(format!(
            "not a usable PBN tag name: {name:?}"
        )));
    }
    Ok(())
}

/// Reject a value that would break out of the single line it is written on, or
/// out of its quotes — either would silently corrupt the file this API exists
/// to leave intact.
fn validate_value(value: &str) -> Result<()> {
    validate_row(value)?;
    if value.contains('"') {
        return Err(ParseError::Pbn(format!(
            "tag value may not contain a double quote: {value:?}"
        )));
    }
    Ok(())
}

/// Reject a data row that would break out of the single line it is written on.
/// Rows are free-form, so unlike a tag value they may contain quotes.
fn validate_row(row: &str) -> Result<()> {
    if row.contains(['\n', '\r']) {
        return Err(ParseError::Pbn(format!(
            "a tag value or data row must be a single line: {row:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with everything that makes naive rewriting lossy: `%` directives,
    /// a `;` comment, a blank line inside `{...}` commentary, a tag with data
    /// rows, and no trailing newline.
    const SAMPLE: &str = concat!(
        "% PBN 2.1\n",
        "% Creator \"Bridge Composer\"\n",
        "; hand-written note\n",
        "\n",
        "[Event \"Club\"]\n",
        "[Board \"1\"]\n",
        "[Dealer \"N\"]\n",
        "[Vulnerable \"None\"]\n",
        "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n",
        "[Result \"9\"]\n",
        "{Cash your winners.\n",
        "\n",
        "Then run the clubs.}\n",
        "\n",
        "[Board \"2\"]\n",
        "[Deal \"E:Q7.AKT9.JT3.JT96 J653.QJ8.A.AQ732 K92.654.K954.K84 AT84.732.Q8762.5\"]\n",
        "[OptimumResultTable \"Declarer;Denomination\\2R;Result\\2R\"]\n",
        "N NT 9\n",
        "N S  8\n",
        "[SkillPath \"notrump/stayman\"]",
    );

    fn open(text: &str) -> PbnDocument {
        PbnDocument::parse(text).expect("parses")
    }

    #[test]
    fn unedited_document_round_trips_byte_for_byte() {
        for text in [
            SAMPLE,
            "",
            "\n\n\n",
            "[Board \"1\"]",
            "\r\n\r\n[Board \"1\"]\r\n[Deal \"N:- - - -\"]\r\n\r\n",
            // Mixed endings, which any lines()-based rewrite would flatten.
            "% header\r\n\n[Board \"1\"]\n[Result \"9\"]\r\n",
        ] {
            let doc = open(text);
            assert_eq!(doc.to_pbn(), text, "round trip of {text:?}");
            assert!(!doc.is_modified());
        }
    }

    #[test]
    fn blocks_tile_the_file_and_boards_match_read_pbn() {
        let doc = open(SAMPLE);
        let expected = read_pbn(SAMPLE).unwrap();
        assert_eq!(doc.boards().len(), expected.len());
        assert_eq!(doc.boards().len(), 2);
        let ids: Vec<_> = doc.boards().iter().map(|b| b.number).collect();
        assert_eq!(ids, vec![Some(1), Some(2)]);
        // The directive block carries no board and is not addressable.
        assert_eq!(doc.board_blocks, vec![1, 2]);
    }

    #[test]
    fn editing_one_board_leaves_every_other_byte_alone() {
        let mut doc = open(SAMPLE);
        doc.set_tag(0, "DoubleDummyTricks", "AAAAAAAAAAAAAAAAAAAA")
            .unwrap();
        let out = doc.to_pbn();

        // Directives, the `;` comment and the untouched board are verbatim.
        assert!(
            out.starts_with("% PBN 2.1\n% Creator \"Bridge Composer\"\n; hand-written note\n\n")
        );
        assert!(out.ends_with("N S  8\n[SkillPath \"notrump/stayman\"]"));
        // The whole file differs from the original by exactly the inserted line.
        assert_eq!(
            out.replace("[DoubleDummyTricks \"AAAAAAAAAAAAAAAAAAAA\"]\n", ""),
            SAMPLE
        );
    }

    #[test]
    fn new_tag_lands_after_the_mandatory_tags_and_before_commentary() {
        let mut doc = open(SAMPLE);
        doc.set_tag(0, "DoubleDummyTricks", "AAAA").unwrap();
        assert!(doc
            .to_pbn()
            .contains("[Result \"9\"]\n[DoubleDummyTricks \"AAAA\"]\n{Cash your winners."));
    }

    #[test]
    fn new_tag_lands_alphabetically_among_supplemental_tags() {
        let mut doc = open(SAMPLE);
        doc.set_tag(1, "DoubleDummyTricks", "AAAA").unwrap();
        // Sorts before OptimumResultTable, so it goes ahead of that whole span.
        assert!(doc
            .to_pbn()
            .contains("Q8762.5\"]\n[DoubleDummyTricks \"AAAA\"]\n[OptimumResultTable "));

        let mut doc = open(SAMPLE);
        doc.set_tag(1, "ParContract", "3NT N").unwrap();
        // Sorts after OptimumResultTable but before SkillPath.
        assert!(doc
            .to_pbn()
            .contains("N S  8\n[ParContract \"3NT N\"]\n[SkillPath "));
    }

    #[test]
    fn new_tag_lands_ahead_of_the_auction_section() {
        let mut doc = open("[Board \"1\"]\n[Auction \"N\"]\n1NT Pass 3NT Pass\nPass Pass\n");
        doc.set_tag(0, "ZTag", "z").unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\n[ZTag \"z\"]\n[Auction \"N\"]\n1NT Pass 3NT Pass\nPass Pass\n"
        );
    }

    #[test]
    fn existing_tag_is_replaced_in_place() {
        let mut doc = open(SAMPLE);
        doc.set_tag(0, "Result", "10").unwrap();
        assert_eq!(
            doc.to_pbn(),
            SAMPLE.replace("[Result \"9\"]", "[Result \"10\"]")
        );
    }

    #[test]
    fn a_section_replaces_its_data_rows_too() {
        let mut doc = open(SAMPLE);
        doc.set_section(
            1,
            "OptimumResultTable",
            "Declarer;Result",
            &["E NT 4", "W  H 7"],
        )
        .unwrap();
        let out = doc.to_pbn();
        assert!(
            out.contains("[OptimumResultTable \"Declarer;Result\"]\nE NT 4\nW  H 7\n[SkillPath ")
        );
        // The rows the old header carried are gone, not left dangling under it.
        assert!(!out.contains("N NT 9"));
        assert!(!out.contains("N S  8"));
    }

    #[test]
    fn removing_a_tag_removes_its_data_rows() {
        let mut doc = open(SAMPLE);
        doc.remove_tag(1, "OptimumResultTable").unwrap();
        assert_eq!(
            doc.to_pbn(),
            SAMPLE
                .replace(
                    "[OptimumResultTable \"Declarer;Denomination\\2R;Result\\2R\"]\n",
                    ""
                )
                .replace("N NT 9\n", "")
                .replace("N S  8\n", "")
        );
    }

    #[test]
    fn re_running_an_edit_is_a_no_op() {
        let mut doc = open(SAMPLE);
        // The value the board already carries.
        doc.set_tag(0, "Result", "9").unwrap();
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), SAMPLE);

        // As is removing a tag that was never there.
        doc.remove_tag(0, "ParContract").unwrap();
        assert!(!doc.is_modified());

        // And setting a tag back to what it was, after changing it.
        doc.set_tag(0, "Result", "10").unwrap();
        assert!(doc.is_modified());
        doc.set_tag(0, "Result", "9").unwrap();
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), SAMPLE);
    }

    #[test]
    fn inserted_lines_take_the_line_ending_around_them() {
        let crlf = "[Board \"1\"]\r\n[Result \"9\"]\r\n";
        let mut doc = open(crlf);
        doc.set_section(0, "OptimumResultTable", "T", &["N NT 9"])
            .unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\r\n[Result \"9\"]\r\n[OptimumResultTable \"T\"]\r\nN NT 9\r\n"
        );

        // A mixed file keeps the ending local to the record being edited.
        let mixed = "% header\n\n[Board \"1\"]\r\n[Result \"9\"]\r\n";
        let mut doc = open(mixed);
        doc.set_tag(0, "ParContract", "3NT N").unwrap();
        assert_eq!(
            doc.to_pbn(),
            "% header\n\n[Board \"1\"]\r\n[Result \"9\"]\r\n[ParContract \"3NT N\"]\r\n"
        );
    }

    #[test]
    fn a_file_ending_without_a_newline_still_does() {
        let mut doc = open(SAMPLE);
        // Sorts after SkillPath, so it appends past the unterminated last line.
        doc.set_tag(1, "ZTag", "z").unwrap();
        let out = doc.to_pbn();
        assert!(out.ends_with("[SkillPath \"notrump/stayman\"]\n[ZTag \"z\"]"));
        assert!(!out.ends_with('\n'));

        // And removing that last line leaves the new last one unterminated.
        let mut doc = open(SAMPLE);
        doc.remove_tag(1, "SkillPath").unwrap();
        assert!(doc.to_pbn().ends_with("N S  8"));
    }

    #[test]
    fn a_blank_line_inside_commentary_does_not_split_a_board() {
        let doc = open(SAMPLE);
        assert_eq!(doc.boards().len(), 2);
        assert_eq!(doc.boards()[0].commentary.len(), 1);
        assert!(doc.boards()[0].commentary[0].contains("Then run the clubs"));
    }

    #[test]
    fn boards_without_a_deal_and_incomplete_deals_pass_through() {
        let text = concat!(
            "[Board \"1\"]\n",
            "[Event \"Teaching\"]\n",
            "\n",
            "[Board \"2\"]\n",
            "[Deal \"W:- KT82.74.AK63.AJ7 - A4.KJ98.T872.865\"]\n",
        );
        let mut doc = open(text);
        assert_eq!(doc.boards().len(), 2);
        assert_eq!(doc.to_pbn(), text);

        // The partial deal is still addressable, and its neighbour untouched.
        doc.set_tag(1, "ZTag", "z").unwrap();
        assert_eq!(doc.to_pbn(), format!("{text}[ZTag \"z\"]\n"));
    }

    #[test]
    fn tags_and_rows_read_back_as_written() {
        let doc = open(SAMPLE);
        assert_eq!(doc.tag(0, "Result"), Some("9"));
        assert_eq!(doc.tag(0, "ParContract"), None);
        assert_eq!(
            doc.tag(1, "OptimumResultTable"),
            Some("Declarer;Denomination\\2R;Result\\2R")
        );
        assert_eq!(
            doc.tag_rows(1, "OptimumResultTable"),
            vec!["N NT 9", "N S  8"]
        );
        assert!(doc.tag_rows(0, "Result").is_empty());
        assert_eq!(doc.tag(2, "Result"), None);
    }

    #[test]
    fn edits_that_could_not_be_written_back_are_errors() {
        let mut doc = open(SAMPLE);
        assert!(doc.set_tag(9, "ZTag", "z").is_err());
        assert!(doc.remove_tag(9, "ZTag").is_err());
        assert!(doc.set_tag(0, "ZTag", "two\nlines").is_err());
        assert!(doc.set_tag(0, "ZTag", "has \"quotes\"").is_err());
        assert!(doc.set_tag(0, "Z Tag", "z").is_err());
        assert!(doc
            .set_section(0, "ZTag", "z", &["fine", "not\nfine"])
            .is_err());
        // None of the rejected edits touched the document.
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), SAMPLE);
    }

    #[test]
    fn splits_lines_losslessly() {
        for text in ["", "\n", "\r\n", "a", "a\n", "a\r\nb", "\n\r\n\na"] {
            let joined: String = split_lines(text)
                .iter()
                .map(|(content, term)| format!("{content}{term}"))
                .collect();
            assert_eq!(joined, text, "split/join of {text:?}");
        }
    }
}
