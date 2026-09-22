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
        // A block carrying no board is still a block a following board's
        // directives came from, so it is carried forward into the next parse.
        // That keeps `boards()` exactly what `read_pbn` returns for the whole
        // file, directives included.
        let mut carried = String::new();
        for (index, range) in ranges.into_iter().enumerate() {
            let block = &text[range.clone()];
            let parsed = if carried.is_empty() {
                read_pbn(block)?
            } else {
                read_pbn(&format!("{carried}{block}"))?
            };
            if parsed.is_empty() {
                carried.push_str(block);
            } else {
                carried.clear();
                for board in parsed {
                    boards.push(board);
                    board_blocks.push(index);
                }
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
    /// pass through untouched, and their directives appear on the board that
    /// follows them, exactly as [`read_pbn`](super::read_pbn) reports them.
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
    /// rows it carried — a file's own tag order is never rearranged, only the
    /// tags this document adds are placed. A new tag is inserted where the
    /// standard's export order puts it: among the mandatory tags in their
    /// listed order, else alphabetically among the supplemental tags, and
    /// always ahead of the `Auction` and `Play` sections.
    ///
    /// Setting a tag to the value it already holds is not a modification; see
    /// [`is_modified`](Self::is_modified).
    ///
    /// A tag is addressed by name, so on a board carrying several tags of that
    /// name only the first is replaced. For the one tag the standard lets repeat
    /// — the auction's `Note`s — use [`set_tags`](Self::set_tags).
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
    /// A tag with rows is a *section*, and a new one is inserted after the
    /// game record — after `[Auction]` and `[Play]`, alphabetically among the
    /// other sections — rather than among the one-line supplemental tags. That
    /// is where PBN 2.1 section 3.1 puts it and where Bridge Composer moves it
    /// to; passing no rows makes this exactly [`set_tag`](Self::set_tag),
    /// placement included.
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
    /// Only the first tag of that name is removed; to clear a repeated tag such
    /// as `Note`, pass no values to [`set_tags`](Self::set_tags).
    ///
    /// Removing a tag the board does not carry is not a modification.
    ///
    /// # Errors
    ///
    /// If `board` is out of range.
    pub fn remove_tag(&mut self, board: usize, name: &str) -> Result<()> {
        self.edit(board, name, None)
    }

    /// Every value of a tag on one board, in file order.
    ///
    /// The reading side of [`set_tags`](Self::set_tags), for the one tag the
    /// standard lets repeat: PBN 2.1 section 3.5.5 says `Note` tags "may occur
    /// more than once (unlike other tags)". [`tag`](Self::tag) returns only the
    /// first.
    pub fn tag_values(&self, board: usize, name: &str) -> Vec<&str> {
        let Some(lines) = self.block_lines(board) else {
            return Vec::new();
        };
        tag_spans(&lines)
            .iter()
            .filter(|span| span.name == name)
            .filter_map(|span| tag_value(lines[span.start].0))
            .collect()
    }

    /// Replace every `name` tag on one board with exactly `values`, written as
    /// one contiguous run of single-line tags.
    ///
    /// This is how to write a repeated tag — the auction's `Note`s — which
    /// [`set_tag`](Self::set_tag) cannot do: it addresses a tag by name, so a
    /// second `set_tag(b, "Note", ..)` replaces the first note instead of adding
    /// one. Here the whole run is replaced, so re-writing an auction with fewer
    /// notes than before leaves none of the old ones behind.
    ///
    /// The run takes the place of the first existing `name` tag, and any others
    /// are removed wherever they stood; a file's own order is otherwise never
    /// rearranged. With none present it is inserted where
    /// [`set_tag`](Self::set_tag) would put the tag — for `Note`, after the
    /// auction's calls and ahead of `[Play]`, as section 3.5.5 requires.
    /// Passing no values removes every `name` tag.
    ///
    /// Writing the values the board already carries is not a modification; see
    /// [`is_modified`](Self::is_modified).
    ///
    /// # Errors
    ///
    /// If `board` is out of range, or `name` or any value could not be written
    /// back as a single well-formed tag line. Every value is checked before
    /// anything is written, so a rejected call leaves the board untouched.
    pub fn set_tags(&mut self, board: usize, name: &str, values: &[&str]) -> Result<()> {
        validate_tag_name(name)?;
        for value in values {
            validate_value(value)?;
        }
        let new: Vec<String> = values
            .iter()
            .map(|value| format!("[{name} \"{value}\"]"))
            .collect();

        self.modify(board, |lines, fallback| {
            let spans = tag_spans(lines);
            let existing: Vec<(usize, usize)> = spans
                .iter()
                .filter(|span| span.name == name)
                .map(|span| (span.start, span.end))
                .collect();

            match existing.first() {
                Some(&(first, _)) => {
                    let newline = own_newline(lines, first, fallback);
                    // Back to front, so each removal leaves the earlier indices
                    // valid.
                    for &(start, end) in existing.iter().rev() {
                        remove_lines(lines, start, end);
                    }
                    insert_lines(lines, first, new, &newline);
                }
                None => {
                    let at = insertion_point(&spans, lines, name, false);
                    let newline = pick_newline(lines, at, fallback).to_string();
                    insert_lines(lines, at, new, &newline);
                }
            }
        })
    }

    /// The text of one board's `{key ...}` comment, after the key and with
    /// surrounding whitespace trimmed, or `None` if the board has none.
    ///
    /// Only a complete single-line brace comment beginning with exactly `key`
    /// matches: `{HCP 10 12 8 10}` for `HCP`, but not `{HCPx}`, not
    /// `{ HCP ...}`, and not a comment spanning several lines. See
    /// [`set_comment`](Self::set_comment).
    pub fn comment(&self, board: usize, key: &str) -> Option<&str> {
        let lines = self.block_lines(board)?;
        let first = *keyed_comments(&lines, key).first()?;
        comment_text(lines[first].0, key)
    }

    /// Ensure one board carries exactly one `{key text}` comment.
    ///
    /// Commentary written by a program — `{Shape 4333 ...}`, `{HCP ...}` — is
    /// identified by its leading keyword, which is what lets a re-run replace
    /// it rather than add a second copy. An existing `{key ...}` comment is
    /// replaced where it stands, and any further copies are removed. Otherwise
    /// the comment is inserted after the `after_tag` tag: PBN 2.1 section 3.8
    /// says a comment "refers to the preceding tag", so commentary about a deal
    /// belongs after `[Deal]`. It goes after any single-line comments already
    /// following that tag, so successive calls keep the order they were made
    /// in. If the board has no `after_tag`, it goes after the board's last tag.
    ///
    /// An empty `text` writes the bare `{key}`. Re-writing the comment a board
    /// already carries is not a modification; see
    /// [`is_modified`](Self::is_modified).
    ///
    /// # Errors
    ///
    /// If `board` is out of range, `after_tag` is not a usable tag name, `key`
    /// is empty or contains whitespace or a brace, or `text` is not a single
    /// line or contains a brace. A brace would end the comment early or make it
    /// unrecognisable as `key`'s, and the next run would add a duplicate.
    pub fn set_comment(
        &mut self,
        board: usize,
        after_tag: &str,
        key: &str,
        text: &str,
    ) -> Result<()> {
        validate_tag_name(after_tag)?;
        validate_comment_key(key)?;
        validate_comment_text(text)?;
        let comment = if text.is_empty() {
            format!("{{{key}}}")
        } else {
            format!("{{{key} {text}}}")
        };

        self.modify(board, |lines, fallback| {
            let existing = keyed_comments(lines, key);
            match existing.first() {
                Some(&first) => {
                    let newline = own_newline(lines, first, fallback);
                    for &index in existing.iter().rev() {
                        remove_lines(lines, index, index + 1);
                    }
                    insert_lines(lines, first, vec![comment], &newline);
                }
                None => {
                    let spans = tag_spans(lines);
                    let at = insertion_point_after(&spans, lines, after_tag, |line| {
                        single_line_comment(line).is_some()
                    });
                    let newline = pick_newline(lines, at, fallback).to_string();
                    insert_lines(lines, at, vec![comment], &newline);
                }
            }
        })
    }

    /// Remove every `{key ...}` comment from one board, matched as
    /// [`comment`](Self::comment) matches them. Any other commentary stays.
    ///
    /// Removing a comment the board does not carry is not a modification.
    ///
    /// # Errors
    ///
    /// If `board` is out of range, or `key` could not be a comment key (see
    /// [`set_comment`](Self::set_comment)).
    pub fn remove_comment(&mut self, board: usize, key: &str) -> Result<()> {
        validate_comment_key(key)?;
        self.modify(board, |lines, _| {
            for &index in keyed_comments(lines, key).iter().rev() {
                remove_lines(lines, index, index + 1);
            }
        })
    }

    /// The text after the `%` of one board's first directive that `recognises`
    /// accepts, trimmed, or `None` if it carries none.
    ///
    /// A directive is a line with `%` in its first column, outside any `{...}`
    /// comment (PBN 2.1 section 3.8). `recognises` is given each directive's
    /// trimmed text. See [`set_directive`](Self::set_directive).
    pub fn directive(&self, board: usize, recognises: impl Fn(&str) -> bool) -> Option<&str> {
        let lines = self.block_lines(board)?;
        let first = *matching_directives(&lines, &recognises).first()?;
        directive_text(lines[first].0)
    }

    /// Ensure one board carries exactly one directive that `recognises`
    /// accepts, written as `% {text}`.
    ///
    /// For directives a program writes into a board and must replace on a
    /// re-run — such as BBA's 28-hex board fingerprint after `[Board]`. Unlike
    /// commentary (see [`set_comment`](Self::set_comment)) such a line may have
    /// no keyword to find it by, since its whole text is the value; so the
    /// caller says what its own directive looks like.
    ///
    /// An existing match is replaced where it stands and any further copies
    /// are removed. Otherwise the directive is inserted after the `after_tag`
    /// tag and any directives already following it, or after the board's last
    /// tag if it has no `after_tag`. Re-writing the directive a board already
    /// carries is not a modification; see [`is_modified`](Self::is_modified).
    ///
    /// # Errors
    ///
    /// If `board` is out of range, `after_tag` is not a usable tag name, `text`
    /// is not a single line — or `recognises` does not accept `text` itself.
    /// That last check is what keeps re-runs honest: a directive its own
    /// predicate cannot see would be invisible to the next run, which would add
    /// a second copy.
    pub fn set_directive(
        &mut self,
        board: usize,
        after_tag: &str,
        text: &str,
        recognises: impl Fn(&str) -> bool,
    ) -> Result<()> {
        validate_tag_name(after_tag)?;
        validate_row(text)?;
        let line = format!("% {}", text.trim());
        // Judge the text exactly as it will be read back.
        if !directive_text(&line).is_some_and(&recognises) {
            return Err(ParseError::Pbn(format!(
                "directive {text:?} is not one its own predicate recognises, so a re-run would duplicate it"
            )));
        }

        self.modify(board, |lines, fallback| {
            let existing = matching_directives(lines, &recognises);
            match existing.first() {
                Some(&first) => {
                    let newline = own_newline(lines, first, fallback);
                    for &index in existing.iter().rev() {
                        remove_lines(lines, index, index + 1);
                    }
                    insert_lines(lines, first, vec![line], &newline);
                }
                None => {
                    let spans = tag_spans(lines);
                    let at = insertion_point_after(&spans, lines, after_tag, |line| {
                        directive_text(line).is_some()
                    });
                    let newline = pick_newline(lines, at, fallback).to_string();
                    insert_lines(lines, at, vec![line], &newline);
                }
            }
        })
    }

    /// Remove every directive `recognises` accepts from one board. Any other
    /// directive stays.
    ///
    /// Removing a directive the board does not carry is not a modification.
    ///
    /// # Errors
    ///
    /// If `board` is out of range.
    pub fn remove_directive(
        &mut self,
        board: usize,
        recognises: impl Fn(&str) -> bool,
    ) -> Result<()> {
        self.modify(board, |lines, _| {
            for &index in matching_directives(lines, &recognises).iter().rev() {
                remove_lines(lines, index, index + 1);
            }
        })
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

    /// Replace, remove or insert one tag span in the block holding `board`.
    fn edit(&mut self, board: usize, name: &str, replacement: Option<Vec<String>>) -> Result<()> {
        self.modify(board, |lines, fallback| {
            let spans = tag_spans(lines);
            let existing = spans.iter().find(|span| span.name == name);

            match (existing, replacement) {
                (Some(span), Some(new)) => {
                    let (start, end) = (span.start, span.end);
                    // Prefer the ending the replaced header already used, so a
                    // file with mixed endings keeps this record's.
                    let newline = own_newline(lines, start, fallback);
                    remove_lines(lines, start, end);
                    insert_lines(lines, start, new, &newline);
                }
                (Some(span), None) => {
                    let (start, end) = (span.start, span.end);
                    remove_lines(lines, start, end);
                }
                (None, Some(new)) => {
                    let at = insertion_point(&spans, lines, name, new.len() > 1);
                    let newline = pick_newline(lines, at, fallback).to_string();
                    insert_lines(lines, at, new, &newline);
                }
                // Removing a tag that is not there changes nothing.
                (None, None) => {}
            }
        })
    }

    /// Apply `change` to the lines of the block holding `board`, then re-render
    /// that block. A render identical to the original bytes clears the edit, so
    /// a change that turns out to be a no-op leaves the document unmodified.
    ///
    /// `change` is infallible by design: callers validate everything first, so
    /// a rejected edit never leaves a board half-modified.
    fn modify(
        &mut self,
        board: usize,
        change: impl FnOnce(&mut Vec<(String, String)>, &str),
    ) -> Result<()> {
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

        change(&mut lines, self.newline);

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
///
/// `str::lines` is the wrong tool for editing a file in place: it discards the
/// terminator, so rejoining with `\n` silently rewrites every CRLF file to LF.
/// Bridge Composer writes CRLF throughout, so that turns "this only adds lines"
/// into "this rewrote your whole file".
///
/// ```
/// use bridge_encodings::pbn::split_lines;
///
/// let lines = split_lines("a\r\nb\n");
/// assert_eq!(lines, vec![("a", "\r\n"), ("b", "\n")]);
/// // Rejoining is exact, mixed endings included.
/// let rejoined: String = lines.iter().map(|(c, t)| format!("{c}{t}")).collect();
/// assert_eq!(rejoined, "a\r\nb\n");
/// ```
pub fn split_lines(text: &str) -> Vec<(&str, &str)> {
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
///
/// ```
/// use bridge_encodings::pbn::prevailing_newline;
///
/// assert_eq!(prevailing_newline("a\r\nb\r\n"), "\r\n");
/// assert_eq!(prevailing_newline("a\nb\n"), "\n");
/// assert_eq!(prevailing_newline(""), "\n");
/// ```
pub fn prevailing_newline(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf > lf {
        "\r\n"
    } else {
        "\n"
    }
}

/// Track `{...}` commentary across a line, following the precedence the
/// standard gives in section 3.8: braces do not nest; a brace inside a `;`
/// rest-of-line comment loses its special meaning, as does one inside a tag
/// pair's quoted value, since a comment may not appear inside a token; and a
/// `;` inside a brace comment is an ordinary character.
fn update_braces(line: &str, mut open: bool) -> bool {
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            // Quotes only delimit a value outside a brace comment.
            '"' if !open => quoted = !quoted,
            '{' if !quoted => open = true,
            '}' if !quoted => open = false,
            // A rest-of-line comment starts here; nothing after it is special.
            ';' if !open && !quoted => break,
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

/// Line indices of a block's `{key ...}` comments: complete single-line brace
/// comments, outside any comment spanning several lines, whose text begins with
/// exactly `key`.
fn keyed_comments<S: AsRef<str>>(lines: &[(S, S)], key: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut in_braces = false;
    for (index, (content, _)) in lines.iter().enumerate() {
        let content = content.as_ref();
        if !in_braces && comment_text(content, key).is_some() {
            found.push(index);
        }
        in_braces = update_braces(content, in_braces);
    }
    found
}

/// The text after `key`, trimmed, when `line` is one complete `{key ...}`
/// comment. The key must be followed by whitespace or the closing brace, so
/// `HCP` does not match `{HCPx}`.
fn comment_text<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = single_line_comment(line)?.strip_prefix(key)?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// The inside of `line` when the whole line is exactly one brace comment.
///
/// Braces do not nest (section 3.8), so a brace anywhere inside means the line
/// holds more than one comment — `{a} {b}` — and is not a single one.
fn single_line_comment(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix('{')?.strip_suffix('}')?;
    (!inner.contains(['{', '}'])).then_some(inner)
}

/// Line indices of a block's directives that `recognises` accepts. A `%` inside
/// a `{...}` comment is an ordinary character (section 3.8), so those lines are
/// skipped.
fn matching_directives<S: AsRef<str>>(
    lines: &[(S, S)],
    recognises: &impl Fn(&str) -> bool,
) -> Vec<usize> {
    let mut found = Vec::new();
    let mut in_braces = false;
    for (index, (content, _)) in lines.iter().enumerate() {
        let content = content.as_ref();
        if !in_braces && directive_text(content).is_some_and(recognises) {
            found.push(index);
        }
        in_braces = update_braces(content, in_braces);
    }
    found
}

/// The trimmed text after the `%` when `line` is a directive — `%` in the first
/// column, as the standard requires.
fn directive_text(line: &str) -> Option<&str> {
    line.strip_prefix('%').map(str::trim)
}

/// The line index to insert a comment or directive at: after the `after_tag`
/// span, or the block's last tag if it has no such tag, then past any lines of
/// the same kind already there — so lines inserted one after another keep the
/// order they were inserted in.
fn insertion_point_after<S: AsRef<str>>(
    spans: &[TagSpan],
    lines: &[(S, S)],
    after_tag: &str,
    same_kind: impl Fn(&str) -> bool,
) -> usize {
    let anchor = spans
        .iter()
        .find(|span| span.name == after_tag)
        .or(spans.last());
    let mut at = anchor.map_or(lines.len(), |span| span.end);
    while lines
        .get(at)
        .is_some_and(|(content, _)| same_kind(content.as_ref()))
    {
        at += 1;
    }
    at
}

/// Sort key deciding where a new tag lands, in the five groups Bridge Composer
/// normalises a file into — see `fixtures/bridge-composer/README.md`, which is
/// the standard's export order (PBN 2.1 sections 3.1 and 3.4) confirmed against
/// the program:
///
/// 1. the 15 mandatory tags, in the order the standard lists them;
/// 2. supplemental tag pairs, alphabetically, unknown names included;
/// 3. `Auction` and its calls, then the `Note` tags explaining them;
/// 4. `Play` and its cards;
/// 5. supplemental sections, alphabetically among themselves.
///
/// `has_rows` is what separates group 2 from group 5, and it is the one thing a
/// name cannot tell you: `OptimumScore` is a one-line summary while
/// `OptimumResultTable` is a header and twenty rows, and without that bit the
/// table sorts in among the summaries, ahead of `[Auction]`, where no exporter
/// puts it.
fn tag_rank(name: &str, has_rows: bool) -> (u8, usize, &str) {
    if let Some(position) = MANDATORY_TAGS.iter().position(|tag| *tag == name) {
        return (0, position, "");
    }
    match name {
        "Auction" => (2, 0, ""),
        // Section 3.5.5: the note tags explaining an auction are placed in the
        // auction section, after its calls, and may not be placed in the
        // identification section. So `Note` sorts after `Auction` and ahead of
        // `Play` rather than among the supplemental tags.
        "Note" => (2, 1, ""),
        "Play" => (3, 0, ""),
        // Section 3.1(4): supplemental sections follow the game record.
        _ if has_rows => (4, 0, name),
        _ => (1, 0, name),
    }
}

/// Whether a tag already in the document carries data rows, and so belongs with
/// the sections rather than the tag pairs. Structural, so a tag is ranked the
/// same on the pass that inserts it and on every pass that reads it back.
fn span_has_rows(span: &TagSpan) -> bool {
    span.end > span.start + 1
}

/// The line index a new `name` tag should be inserted at. `has_rows` says
/// whether the caller is writing a section or a bare tag pair.
fn insertion_point<S: AsRef<str>>(
    spans: &[TagSpan],
    lines: &[(S, S)],
    name: &str,
    has_rows: bool,
) -> usize {
    let rank = tag_rank(name, has_rows);
    // Anchor on the last tag, in file order, that ranks at or before the new
    // one, and insert ahead of the tag that follows it. In a file already in
    // export order that is exactly "before the first tag ranking after it";
    // the difference is a file that is not. Practice-Bidding-Scenarios writes
    // `[HandType]` straight after `[Board]`, and "before the first tag ranking
    // after it" would put every later mandatory tag — `Scoring`, say — up
    // there too, ahead of the players and the deal.
    //
    // Inserting ahead of the following tag, rather than straight after the
    // anchor, keeps commentary with the tag it refers to (section 3.8).
    if let Some(anchor) = spans
        .iter()
        .rposition(|span| tag_rank(&span.name, span_has_rows(span)) <= rank)
    {
        return match spans.get(anchor + 1) {
            Some(next) => next.start,
            // After every tag, but ahead of any trailing commentary or blank
            // lines.
            None => spans[anchor].end,
        };
    }
    if let Some(first) = spans.first() {
        return first.start;
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

/// The line ending to give lines replacing the one at `at`: that line's own, so
/// a record in a file with mixed endings keeps its ending, else whatever
/// [`pick_newline`] finds around it.
fn own_newline(lines: &[(String, String)], at: usize, fallback: &str) -> String {
    match lines.get(at).map(|(_, term)| term.as_str()) {
        Some(own) if !own.is_empty() => own.to_string(),
        _ => pick_newline(lines, at, fallback).to_string(),
    }
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

/// Reject a comment key that could not be found again by
/// [`PbnDocument::comment`]: the key is the comment's first word, so it cannot
/// be empty or contain whitespace, and a brace would end or split the comment.
fn validate_comment_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '{' | '}'))
    {
        return Err(ParseError::Pbn(format!(
            "not a usable comment key: {key:?}"
        )));
    }
    Ok(())
}

/// Reject comment text that would break out of its line, or contain a brace —
/// a `}` ends the comment early, and either brace makes the line read as more
/// than one comment, so the next run would not recognise it and would add a
/// duplicate.
fn validate_comment_text(text: &str) -> Result<()> {
    validate_row(text)?;
    if text.contains(['{', '}']) {
        return Err(ParseError::Pbn(format!(
            "comment text may not contain a brace: {text:?}"
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
        // A one-line tag sorts among the tag pairs, so it goes ahead of the
        // OptimumResultTable *section* even though `P` sorts after `O`.
        assert!(doc
            .to_pbn()
            .contains("Q8762.5\"]\n[ParContract \"3NT N\"]\n[OptimumResultTable "));
    }

    #[test]
    fn a_new_section_lands_after_the_game_record() {
        // The bug this replaced: OptimumResultTable and its twenty rows were
        // ranked as though they were a one-line tag, so the table landed
        // between DoubleDummyTricks and OptimumScore, ahead of the auction.
        let src = concat!(
            "[Board \"1\"]\n",
            "[Auction \"N\"]\n",
            "1NT Pass 3NT AP\n",
            "[Play \"W\"]\n",
            "S2 S3 S4 SA\n",
        );
        let mut doc = open(src);
        doc.set_section(0, "OptimumResultTable", "Declarer;Result", &["N NT 9"])
            .unwrap();
        doc.set_tag(0, "OptimumScore", "NS 400").unwrap();
        doc.set_tag(0, "DoubleDummyTricks", "AAAA").unwrap();
        assert_eq!(
            doc.to_pbn(),
            concat!(
                "[Board \"1\"]\n",
                "[DoubleDummyTricks \"AAAA\"]\n",
                "[OptimumScore \"NS 400\"]\n",
                "[Auction \"N\"]\n",
                "1NT Pass 3NT AP\n",
                "[Play \"W\"]\n",
                "S2 S3 S4 SA\n",
                "[OptimumResultTable \"Declarer;Result\"]\n",
                "N NT 9\n",
            )
        );

        // And sections sort alphabetically among themselves, not by the order
        // the calls were made in.
        doc.set_section(0, "AAATable", "Declarer;Result", &["N NT 9"])
            .unwrap();
        assert!(doc.to_pbn().contains(concat!(
            "S2 S3 S4 SA\n",
            "[AAATable \"Declarer;Result\"]\n",
            "N NT 9\n",
            "[OptimumResultTable \"Declarer;Result\"]\n",
        )));
    }

    #[test]
    fn a_note_stays_inside_the_auction_section() {
        // Standard 3.5.5: the notes explaining an auction are placed in the
        // auction section, after its calls, and may not be placed in the
        // identification section. So a Note goes after `[Auction]` and ahead of
        // `[Play]` — and ahead of the supplemental sections that follow both.
        let src = concat!(
            "[Board \"1\"]\n",
            "[Auction \"N\"]\n",
            "1NT =1= Pass 3NT AP\n",
            "[Play \"W\"]\n",
            "S2 S3 S4 SA\n",
            "[OptimumResultTable \"Declarer;Result\"]\n",
            "N NT 9\n",
        );
        let mut doc = open(src);
        doc.set_tag(0, "Note", "1:15-17 balanced").unwrap();
        assert_eq!(
            doc.to_pbn(),
            src.replace("[Play \"W\"]", "[Note \"1:15-17 balanced\"]\n[Play \"W\"]")
        );
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
        // A section sorting after OptimumResultTable goes last of all, so it
        // appends past the unterminated last line.
        doc.set_section(1, "ZTable", "Declarer;Result", &["W  H 7"])
            .unwrap();
        let out = doc.to_pbn();
        assert!(
            out.ends_with("[SkillPath \"notrump/stayman\"]\n[ZTable \"Declarer;Result\"]\nW  H 7")
        );
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

    #[test]
    fn a_brace_inside_a_semicolon_comment_is_an_ordinary_character() {
        // Standard 3.8: "Braces appearing inside of semicolon comments lose
        // their special meaning and are ignored." Treating one as commentary
        // would swallow the blank line and merge two records into one block,
        // and an edit aimed at the second board would land on the first.
        let src = "[Board \"1\"]\n; a note with { an unmatched brace\n\n[Board \"2\"]\n";
        let mut doc = open(src);
        assert_eq!(doc.boards().len(), 2);
        doc.set_tag(1, "Result", "9").unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\n; a note with { an unmatched brace\n\n[Board \"2\"]\n[Result \"9\"]\n"
        );
    }

    #[test]
    fn a_brace_or_semicolon_inside_a_tag_value_is_an_ordinary_character() {
        // A comment may not appear inside a token (3.8), and real files carry
        // both: OptimumResultTable's value is semicolon-separated.
        let src = concat!(
            "[OptimumResultTable \"Declarer;Denomination\\2R;Result\\2R\"]\n",
            "N NT 9\n",
            "[Event \"a { brace in a value\"]\n",
            "\n",
            "[Board \"2\"]\n",
        );
        let doc = open(src);
        assert_eq!(doc.boards().len(), 2);
        assert_eq!(doc.tag_rows(0, "OptimumResultTable"), vec!["N NT 9"]);
        assert_eq!(doc.to_pbn(), src);
    }

    /// The file Bridge Composer 5.118.2 was given: eight boards, the mandatory
    /// tags deliberately shuffled and the supplemental tags put somewhere
    /// different in each. See `fixtures/bridge-composer/README.md`.
    const BC_INPUT: &str = include_str!("../../fixtures/bridge-composer/pbn-order-test.pbn");

    /// What Bridge Composer saved after opening that file and taking a trivial
    /// edit. It restored the mandatory tags to the standard's order, which is
    /// the control proving the rest of its layout is normalisation and not the
    /// input surviving.
    const BC_OUTPUT: &str = include_str!("../../fixtures/bridge-composer/pbn-order-test-bc.pbn");

    /// A tag whose placement this crate decides: not mandatory, and not part of
    /// the game record, whose position is fixed by what it describes.
    fn is_supplemental(name: &str) -> bool {
        !MANDATORY_TAGS.contains(&name) && !matches!(name, "Auction" | "Play" | "Note")
    }

    /// The tags of one board whose relative order this crate claims to decide:
    /// the supplemental ones and the game record they sort around. `BCFlags` is
    /// Bridge Composer's own bookkeeping, absent from the input, so it is not
    /// part of the comparison.
    fn placement_order(doc: &PbnDocument, board: usize) -> Vec<String> {
        let lines = doc.block_lines(board).expect("board in range");
        tag_spans(&lines)
            .into_iter()
            .map(|span| span.name)
            .filter(|name| !MANDATORY_TAGS.contains(&name.as_str()) && name != "BCFlags")
            .collect()
    }

    #[test]
    fn bridge_composer_output_is_already_in_rank_order() {
        // The oracle read directly: whatever Bridge Composer wrote, `tag_rank`
        // must already agree with, board by board and tag by tag.
        let doc = open(BC_OUTPUT);
        assert_eq!(doc.boards().len(), 9, "a template board, then the eight");
        for board in 0..doc.boards().len() {
            let lines = doc.block_lines(board).expect("board in range");
            let spans = tag_spans(&lines);
            let ranks: Vec<_> = spans
                .iter()
                .map(|span| tag_rank(&span.name, span_has_rows(span)))
                .collect();
            let mut sorted = ranks.clone();
            sorted.sort_unstable();
            assert_eq!(ranks, sorted, "board {board} is not in tag_rank order");
        }
    }

    #[test]
    fn reinserting_every_supplemental_tag_reproduces_bridge_composers_order() {
        // Strip each board of every tag whose placement is ours to decide, put
        // them all back in reverse-alphabetical order, and the result must be
        // the order Bridge Composer produced from the same input.
        let expected = open(BC_OUTPUT);
        let mut doc = open(BC_INPUT);
        assert_eq!(doc.boards().len(), 8);
        assert_eq!(expected.boards().len(), 9, "BC prepends a template board");

        for board in 0..doc.boards().len() {
            let mut supplemental: Vec<(String, String, Vec<String>)> = {
                let lines = doc.block_lines(board).expect("board in range");
                tag_spans(&lines)
                    .iter()
                    .filter(|span| is_supplemental(&span.name))
                    .map(|span| {
                        let value = tag_value(lines[span.start].0).unwrap_or_default();
                        let rows = lines[span.start + 1..span.end]
                            .iter()
                            .map(|(content, _)| (*content).to_string())
                            .collect();
                        (span.name.clone(), value.to_string(), rows)
                    })
                    .collect()
            };
            for (name, _, _) in &supplemental {
                doc.remove_tag(board, name).expect("board in range");
            }
            supplemental.sort_by(|left, right| right.0.cmp(&left.0));
            for (name, value, rows) in &supplemental {
                let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                doc.set_section(board, name, value, &rows)
                    .expect("a tag taken from the file can be written back");
            }
        }

        for board in 0..doc.boards().len() {
            assert_eq!(
                placement_order(&doc, board),
                placement_order(&expected, board + 1),
                "board {} is not where Bridge Composer put it",
                board + 1
            );
        }

        // Concretely, on the two boards built to probe the edges: unknown
        // one-line tags sort among the known ones, and a custom section sorts
        // with the sections, after the auction.
        assert_eq!(
            placement_order(&doc, 6),
            [
                "AAACustom",
                "DoubleDummyTricks",
                "OptimumScore",
                "ParContract",
                "ZZZCustom",
                "Auction",
                "OptimumResultTable"
            ]
        );
        assert_eq!(
            placement_order(&doc, 7),
            [
                "DoubleDummyTricks",
                "OptimumScore",
                "ParContract",
                "Auction",
                "AAATable",
                "OptimumResultTable"
            ]
        );
    }

    #[test]
    fn a_tag_already_in_the_file_is_never_moved() {
        // Load-bearing: re-annotating a Bridge Composer file must not rewrite
        // the order it chose, and real files are not self-consistent. Setting
        // every supplemental tag of the fixture to the value it already holds
        // must leave the whole file byte-for-byte unchanged, wherever the tags
        // happen to sit.
        let mut doc = open(BC_INPUT);
        for board in 0..doc.boards().len() {
            let supplemental: Vec<(String, String, Vec<String>)> = {
                let lines = doc.block_lines(board).expect("board in range");
                tag_spans(&lines)
                    .iter()
                    .filter(|span| is_supplemental(&span.name))
                    .map(|span| {
                        let value = tag_value(lines[span.start].0).unwrap_or_default();
                        let rows = lines[span.start + 1..span.end]
                            .iter()
                            .map(|(content, _)| (*content).to_string())
                            .collect();
                        (span.name.clone(), value.to_string(), rows)
                    })
                    .collect()
            };
            for (name, value, rows) in &supplemental {
                let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                doc.set_section(board, name, value, &rows)
                    .expect("a tag taken from the file can be written back");
            }
        }
        assert!(
            !doc.is_modified(),
            "a no-op re-annotation modified the file"
        );
        assert_eq!(doc.to_pbn(), BC_INPUT);
    }

    #[test]
    fn a_semicolon_inside_brace_commentary_stays_commentary() {
        // The converse rule: "A semicolon appearing inside of a brace comment
        // loses its special meaning", so it must not stop the brace scan and
        // let the blank line split the record.
        let src = "[Board \"1\"]\n{open ; semicolon\n\nstill open}\n\n[Board \"2\"]\n";
        let doc = open(src);
        assert_eq!(doc.boards().len(), 2);
        assert_eq!(doc.to_pbn(), src);
    }

    /// A board as bba-cli writes it: a deal described by keyed commentary, an
    /// auction with its notes, and tags and a section it does not own.
    const BID: &str = concat!(
        "[Board \"1\"]\n",
        "[HandType \"Game\"]\n",
        "[Dealer \"N\"]\n",
        "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n",
        "{Shape 4432 4135 3343 2452}\n",
        "{HCP 4 16 9 11}\n",
        "[Contract \"3NT\"]\n",
        "[Auction \"N\"]\n",
        "Pass 1C =1= Pass 1NT =2=\n",
        "Pass 3NT AP\n",
        "[Note \"1:3+ clubs\"]\n",
        "[Note \"2:12-14\"]\n",
        "[OptimumResultTable \"Declarer;Result\"]\n",
        "N NT 9\n",
    );

    #[test]
    fn set_tags_writes_several_notes_after_the_calls() {
        // What set_tag cannot do: a second Note would replace the first.
        let src = concat!(
            "[Board \"1\"]\n",
            "[Auction \"N\"]\n",
            "1NT =1= Pass 2C =2=\n",
            "Pass 2D Pass 3NT\n",
            "AP\n",
            "[Play \"W\"]\n",
            "S2 S3 S4 SA\n",
        );
        let mut doc = open(src);
        doc.set_tags(0, "Note", &["1:15-17", "2:Stayman"]).unwrap();
        assert_eq!(
            doc.to_pbn(),
            src.replace(
                "[Play \"W\"]",
                "[Note \"1:15-17\"]\n[Note \"2:Stayman\"]\n[Play \"W\"]"
            )
        );
        assert_eq!(doc.tag_values(0, "Note"), vec!["1:15-17", "2:Stayman"]);
    }

    #[test]
    fn set_tags_replaces_the_whole_run_leaving_no_stale_tag() {
        let old_notes = "[Note \"1:3+ clubs\"]\n[Note \"2:12-14\"]\n";

        let mut fewer = open(BID);
        fewer.set_tags(0, "Note", &["1:Precision"]).unwrap();
        assert_eq!(
            fewer.to_pbn(),
            BID.replace(old_notes, "[Note \"1:Precision\"]\n")
        );

        let mut more = open(BID);
        more.set_tags(0, "Note", &["1:a", "2:b", "3:c"]).unwrap();
        assert_eq!(
            more.to_pbn(),
            BID.replace(
                old_notes,
                "[Note \"1:a\"]\n[Note \"2:b\"]\n[Note \"3:c\"]\n"
            )
        );
    }

    #[test]
    fn set_tags_gathers_scattered_tags_where_the_first_stood() {
        let src = concat!(
            "[Board \"1\"]\n",
            "[Auction \"N\"]\n",
            "1NT =1= AP\n",
            "[Note \"1:a\"]\n",
            "[Play \"W\"]\n",
            "S2 S3 S4 SA\n",
            "[Note \"2:b\"]\n",
        );
        let mut doc = open(src);
        doc.set_tags(0, "Note", &["1:x", "2:y"]).unwrap();
        assert_eq!(
            doc.to_pbn(),
            concat!(
                "[Board \"1\"]\n",
                "[Auction \"N\"]\n",
                "1NT =1= AP\n",
                "[Note \"1:x\"]\n",
                "[Note \"2:y\"]\n",
                "[Play \"W\"]\n",
                "S2 S3 S4 SA\n",
            )
        );
    }

    #[test]
    fn set_tags_with_no_values_removes_every_one() {
        let mut doc = open(BID);
        doc.set_tags(0, "Note", &[]).unwrap();
        assert_eq!(
            doc.to_pbn(),
            BID.replace("[Note \"1:3+ clubs\"]\n[Note \"2:12-14\"]\n", "")
        );
        assert!(doc.tag_values(0, "Note").is_empty());

        // Where remove_tag, addressing a tag by name, takes only the first.
        let mut first_only = open(BID);
        first_only.remove_tag(0, "Note").unwrap();
        assert_eq!(first_only.tag_values(0, "Note"), vec!["2:12-14"]);
    }

    #[test]
    fn re_writing_the_same_tags_is_a_no_op() {
        let mut doc = open(BID);
        doc.set_tags(0, "Note", &["1:3+ clubs", "2:12-14"]).unwrap();
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), BID);

        // Clearing a tag the board never had.
        let mut plain = open(SAMPLE);
        plain.set_tags(0, "Note", &[]).unwrap();
        assert!(!plain.is_modified());
    }

    #[test]
    fn a_rejected_value_leaves_the_board_untouched() {
        let mut doc = open(BID);
        // The first value is fine; the second would break out of its quotes.
        assert!(doc.set_tags(0, "Note", &["1:ok", "2:a \"quote\""]).is_err());
        assert!(doc.set_tags(0, "Note", &["1:two\nlines"]).is_err());
        assert!(doc.set_tags(0, "Bad Name", &["x"]).is_err());
        assert!(doc.set_tags(9, "Note", &["1:x"]).is_err());
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), BID);
    }

    #[test]
    fn set_tags_keeps_crlf_and_a_missing_final_newline() {
        let src = "[Board \"1\"]\r\n[Auction \"N\"]\r\n1NT =1= AP\r\n[Note \"1:old\"]";
        let mut doc = open(src);
        doc.set_tags(0, "Note", &["1:a", "2:b"]).unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\r\n[Auction \"N\"]\r\n1NT =1= AP\r\n[Note \"1:a\"]\r\n[Note \"2:b\"]"
        );
    }

    #[test]
    fn set_comment_inserts_after_its_tag_in_call_order() {
        let src = concat!(
            "[Board \"1\"]\n",
            "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n",
            "[Contract \"3NT\"]\n",
        );
        let mut doc = open(src);
        doc.set_comment(0, "Deal", "Shape", "4432 4135 3343 2452")
            .unwrap();
        doc.set_comment(0, "Deal", "HCP", "4 16 9 11").unwrap();
        doc.set_comment(0, "Deal", "Losers", "9 5 8 7").unwrap();
        assert_eq!(
            doc.to_pbn(),
            src.replace(
                "[Contract ",
                "{Shape 4432 4135 3343 2452}\n{HCP 4 16 9 11}\n{Losers 9 5 8 7}\n[Contract "
            )
        );
        assert_eq!(doc.comment(0, "HCP"), Some("4 16 9 11"));
    }

    #[test]
    fn set_comment_replaces_in_place_and_collapses_duplicates() {
        let src = concat!(
            "[Board \"1\"]\n",
            "[Result \"9\"]\n",
            "{HCP 1 2 3 4}\n",
            "{Hand-written: nice play.}\n",
            "[Contract \"3NT\"]\n",
            "{HCP 9 9 9 9}\n",
        );
        let mut doc = open(src);
        // The anchor is irrelevant once the comment exists: it is never moved.
        doc.set_comment(0, "Deal", "HCP", "10 11 9 10").unwrap();
        assert_eq!(
            doc.to_pbn(),
            concat!(
                "[Board \"1\"]\n",
                "[Result \"9\"]\n",
                "{HCP 10 11 9 10}\n",
                "{Hand-written: nice play.}\n",
                "[Contract \"3NT\"]\n",
            )
        );
    }

    #[test]
    fn re_writing_a_comment_is_a_no_op() {
        let mut doc = open(BID);
        doc.set_comment(0, "Deal", "Shape", "4432 4135 3343 2452")
            .unwrap();
        doc.set_comment(0, "Deal", "HCP", "4 16 9 11").unwrap();
        assert!(!doc.is_modified());

        // Removing one the board does not carry.
        doc.remove_comment(0, "Losers").unwrap();
        assert!(!doc.is_modified());
        assert_eq!(doc.to_pbn(), BID);
    }

    #[test]
    fn only_a_whole_single_line_comment_with_that_exact_key_matches() {
        let src = concat!(
            "[Board \"1\"]\n",
            "{HCPx 1}\n",
            "{ HCP 2}\n",
            "{HCP 3 spans\n",
            "two lines}\n",
            "{Note:\n",
            "{HCP 4}\n",
            "{a} {HCP 5}\n",
            "[Result \"9\"]\n",
            "{HCP 6}\n",
        );
        let mut doc = open(src);
        assert_eq!(doc.comment(0, "HCP"), Some("6"));
        doc.remove_comment(0, "HCP").unwrap();
        // Everything but the one genuine `{HCP ...}` comment is left alone —
        // including `{HCP 4}`, which sits inside the comment opened by `{Note:`.
        assert_eq!(doc.to_pbn(), src.replace("{HCP 6}\n", ""));

        let bare = open("[Board \"1\"]\n{Alert}\n");
        assert_eq!(bare.comment(0, "Alert"), Some(""));
    }

    #[test]
    fn a_comment_whose_tag_is_missing_goes_after_the_last_tag() {
        let mut doc = open("[Board \"1\"]\n[Result \"9\"]\n; trailing remark\n");
        doc.set_comment(0, "Deal", "HCP", "1 2 3 4").unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\n[Result \"9\"]\n{HCP 1 2 3 4}\n; trailing remark\n"
        );
    }

    #[test]
    fn a_rejected_comment_leaves_the_board_untouched() {
        let mut doc = open(BID);
        for (after, key, text) in [
            ("Deal", "", "x"),
            ("Deal", "two words", "x"),
            ("Deal", "a{b", "x"),
            ("Deal", "HCP", "ends } early"),
            ("Deal", "HCP", "{nested"),
            ("Deal", "HCP", "two\nlines"),
            ("bad tag", "HCP", "x"),
        ] {
            assert!(
                doc.set_comment(0, after, key, text).is_err(),
                "{after:?} {key:?} {text:?}"
            );
        }
        assert!(doc.remove_comment(0, "").is_err());
        assert!(doc.set_comment(9, "Deal", "HCP", "x").is_err());
        assert!(!doc.is_modified());
    }

    #[test]
    fn re_bidding_a_board_replaces_what_the_bidder_owns_and_nothing_else() {
        // The whole of what bba-cli does to a board, run twice: the first pass
        // replaces its auction, notes and commentary and leaves HandType and the
        // OptimumResultTable exactly as they were; the second pass is a no-op.
        fn rebid(doc: &mut PbnDocument) -> Result<()> {
            doc.set_comment(0, "Deal", "Shape", "4432 4135 3343 2452")?;
            doc.set_comment(0, "Deal", "HCP", "4 16 9 11")?;
            doc.set_comment(0, "Deal", "Losers", "9 5 8 7")?;
            doc.set_tag(0, "Contract", "4H")?;
            doc.set_section(0, "Auction", "N", &["Pass 1C =1= 1H 4H", "AP"])?;
            doc.set_tags(0, "Note", &["1:Precision"])
        }

        let mut doc = open(BID);
        rebid(&mut doc).unwrap();
        let once = doc.to_pbn();
        assert_eq!(
            once,
            concat!(
                "[Board \"1\"]\n",
                "[HandType \"Game\"]\n",
                "[Dealer \"N\"]\n",
                "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n",
                "{Shape 4432 4135 3343 2452}\n",
                "{HCP 4 16 9 11}\n",
                "{Losers 9 5 8 7}\n",
                "[Contract \"4H\"]\n",
                "[Auction \"N\"]\n",
                "Pass 1C =1= 1H 4H\n",
                "AP\n",
                "[Note \"1:Precision\"]\n",
                "[OptimumResultTable \"Declarer;Result\"]\n",
                "N NT 9\n",
            )
        );

        let mut again = open(&once);
        rebid(&mut again).unwrap();
        assert!(!again.is_modified());
        assert_eq!(again.to_pbn(), once);
    }

    /// BBA's board fingerprint: the whole directive is 28 hex digits, with no
    /// keyword to find it by.
    fn is_bba_hash(text: &str) -> bool {
        text.len() == 28 && text.chars().all(|c| c.is_ascii_hexdigit())
    }

    const HASH: &str = "000B6835D55DDDE2A07889A2F0DF";

    #[test]
    fn set_directive_inserts_after_its_tag() {
        let mut doc = open("[Board \"1\"]\n[North \"-\"]\n");
        doc.set_directive(0, "Board", HASH, is_bba_hash).unwrap();
        assert_eq!(
            doc.to_pbn(),
            format!("[Board \"1\"]\n% {HASH}\n[North \"-\"]\n")
        );
        assert_eq!(doc.directive(0, is_bba_hash), Some(HASH));
    }

    #[test]
    fn set_directive_replaces_in_place_and_leaves_other_directives_alone() {
        let src = concat!(
            "%HRTitleEvent \"1N\"\n",
            "[Board \"1\"]\n",
            "% AAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "[North \"-\"]\n",
            "% BBBBBBBBBBBBBBBBBBBBBBBBBBBB\n",
        );
        let mut doc = open(src);
        doc.set_directive(0, "Board", HASH, is_bba_hash).unwrap();
        let once = doc.to_pbn();
        assert_eq!(
            once,
            format!("%HRTitleEvent \"1N\"\n[Board \"1\"]\n% {HASH}\n[North \"-\"]\n")
        );

        // The re-run a bba/ file gets: identical, so not a modification.
        let mut again = open(&once);
        again.set_directive(0, "Board", HASH, is_bba_hash).unwrap();
        assert!(!again.is_modified());
    }

    #[test]
    fn a_directive_its_own_predicate_would_miss_is_rejected() {
        let mut doc = open("[Board \"1\"]\n[North \"-\"]\n");
        // Written, this would be invisible to the next run and get duplicated.
        assert!(doc
            .set_directive(0, "Board", "not a hash", is_bba_hash)
            .is_err());
        assert!(doc
            .set_directive(0, "Board", "two\nlines", |_| true)
            .is_err());
        assert!(doc.set_directive(0, "bad tag", HASH, is_bba_hash).is_err());
        assert!(doc.set_directive(9, "Board", HASH, is_bba_hash).is_err());
        assert!(!doc.is_modified());
    }

    #[test]
    fn a_percent_inside_commentary_or_off_column_one_is_not_a_directive() {
        let src = concat!(
            "[Board \"1\"]\n",
            "{A remark\n",
            "% AAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "still the remark}\n",
            " % BBBBBBBBBBBBBBBBBBBBBBBBBBBB\n",
        );
        let mut doc = open(src);
        assert_eq!(doc.directive(0, is_bba_hash), None);
        doc.remove_directive(0, is_bba_hash).unwrap();
        assert!(!doc.is_modified());
    }

    #[test]
    fn remove_directive_removes_every_match_and_nothing_else() {
        let src = concat!(
            "[Board \"1\"]\n",
            "% AAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "%HRTitleEvent \"1N\"\n",
            "[North \"-\"]\n",
            "% BBBBBBBBBBBBBBBBBBBBBBBBBBBB\n",
        );
        let mut doc = open(src);
        doc.remove_directive(0, is_bba_hash).unwrap();
        assert_eq!(
            doc.to_pbn(),
            "[Board \"1\"]\n%HRTitleEvent \"1N\"\n[North \"-\"]\n"
        );
    }

    #[test]
    fn inserted_directives_keep_call_order_line_endings_and_a_missing_anchor() {
        let mut doc = open("[Board \"1\"]\r\n[North \"-\"]\r\n");
        doc.set_directive(0, "Board", HASH, is_bba_hash).unwrap();
        doc.set_directive(0, "Board", "Seed 7", |t| t.starts_with("Seed "))
            .unwrap();
        assert_eq!(
            doc.to_pbn(),
            format!("[Board \"1\"]\r\n% {HASH}\r\n% Seed 7\r\n[North \"-\"]\r\n")
        );

        let mut no_anchor = open("[Deal \"N:- - - -\"]\n");
        no_anchor
            .set_directive(0, "Board", HASH, is_bba_hash)
            .unwrap();
        assert_eq!(
            no_anchor.to_pbn(),
            format!("[Deal \"N:- - - -\"]\n% {HASH}\n")
        );
    }

    #[test]
    fn a_new_tag_keeps_to_its_group_in_a_file_out_of_export_order() {
        // Practice-Bidding-Scenarios' leveled deals carry `[HandType]` straight
        // after `[Board]`, ahead of the mandatory tags it should follow. New
        // tags must still land in their own group, not be dragged up with it.
        let src = concat!(
            "[Board \"1\"]\n",
            "[HandType \"Game\"]\n",
            "[West \"-\"]\n",
            "[Deal \"N:- - - -\"]\n",
            "{Shape 4333 4333 4333 4333}\n",
            "[Declarer \"?\"]\n",
            "[Result \"?\"]\n",
            "[OptimumResultTable \"Declarer;Result\"]\n",
            "N NT 9\n",
        );
        let mut doc = open(src);
        doc.set_tag(0, "Scoring", "MP").unwrap();
        doc.set_tag(0, "BidSystemNS", "2/1").unwrap();
        doc.set_section(0, "Auction", "N", &["1NT AP"]).unwrap();
        assert_eq!(
            doc.to_pbn(),
            concat!(
                "[Board \"1\"]\n",
                "[HandType \"Game\"]\n",
                "[West \"-\"]\n",
                "[Deal \"N:- - - -\"]\n",
                // After the deal's commentary, which refers to the deal.
                "{Shape 4333 4333 4333 4333}\n",
                "[Scoring \"MP\"]\n",
                "[Declarer \"?\"]\n",
                "[Result \"?\"]\n",
                "[BidSystemNS \"2/1\"]\n",
                "[Auction \"N\"]\n",
                "1NT AP\n",
                "[OptimumResultTable \"Declarer;Result\"]\n",
                "N NT 9\n",
            )
        );
    }
}
