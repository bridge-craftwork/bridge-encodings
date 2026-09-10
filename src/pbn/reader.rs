//! PBN file reader.
//!
//! Section-aware: besides the scalar tags, it parses the `Auction` and `Play`
//! sections into typed `Auction`/`PlaySequence`, decodes an
//! `OptimumResultTable` section into the board's double-dummy table, captures
//! `{...}` commentary blocks, and preserves every tag it does not otherwise
//! model as an `extra_tags` pair on the board (the PBN spec permits arbitrary
//! supplemental tags; dropping them is lossy). `%` directives and `;` comments are kept too,
//! each anchored to the tag it followed, so the writer can put them back where
//! their author had them. Board records are terminated by a blank line, per the
//! PBN standard.

use crate::error::Result;
use bridge_types::{
    Auction, Board, Call, Card, Deal, Direction, Directive, PlaySequence, PlayerNames, Rank,
    SectionEnd, Strain, Suit, Vulnerability,
};

/// A parsed PBN tag pair
#[derive(Debug, Clone)]
pub struct TagPair {
    pub name: String,
    pub value: String,
}

/// Parse a tag pair from a line: [TagName "value"]
///
/// Returns `None` for anything that is not a well-formed tag line, so it also
/// serves as the test for whether a line is one. A rest-of-line comment after
/// the tag does not stop it being one; use [`parse_tag_line`] to get the
/// comment as well.
pub fn parse_tag_pair(line: &str) -> Option<TagPair> {
    parse_tag_line(line).map(|(tag, _)| tag)
}

/// Parse a tag pair together with any comment trailing it on the same line.
///
/// A `;` comment runs to the end of the line and a `{...}` comment may sit
/// after a tag, and the standard says a comment "refers to the preceding tag" —
/// so `[Board "1"] ; the first board` is a tag *and* a comment, not a malformed
/// line. Requiring the line to end in `]` lost both.
///
/// The tag is taken to end at the first `]` whose preceding character is the
/// value's closing quote, which keeps a bracket or quote inside a trailing
/// comment from being mistaken for the tag's own. Values are returned exactly
/// as written: the standard escapes a quote inside a value as `\"`, but real
/// files also use a bare backslash in `OptimumResultTable` column widths, so
/// decoding escapes here would corrupt them.
pub fn parse_tag_line(line: &str) -> Option<(TagPair, Option<&str>)> {
    let line = line.trim();
    let body = line.strip_prefix('[')?;

    let close = body
        .char_indices()
        .find(|(i, c)| *c == ']' && body[..*i].trim_end().ends_with('"'))
        .map(|(i, _)| i)?;
    let inner = &body[..close];
    let trailing = body[close + 1..].trim();

    // Split the tag name from its quoted value.
    let space_pos = inner.find(char::is_whitespace)?;
    let name = inner[..space_pos].trim().to_string();
    let rest = inner[space_pos..].trim();
    let value = rest.strip_prefix('"')?.strip_suffix('"')?.to_string();

    // Only a comment may follow a tag on its line. Anything else means this is
    // not a tag line after all — files exist that write `[Play "W"]S2`, jamming
    // section data onto the tag, and calling that a comment would invent one.
    // A `{` comment counts only when it closes on the same line; an open one
    // spans lines and belongs to the commentary scanner, not here.
    let comment = match trailing.chars().next() {
        None => None,
        Some(';') => Some(trailing),
        Some('{') if trailing.contains('}') => Some(trailing),
        Some(_) => return None,
    };
    Some((TagPair { name, value }, comment))
}

/// Mutable parse state carried across lines within one file.
#[derive(Default)]
struct ParseState {
    board: Board,
    has_content: bool,
    in_commentary: bool,
    commentary_buf: Vec<String>,
    // Auction/Play sections: the tag opens the section; following data lines
    // accumulate until the next tag or a blank line closes it.
    auction_dealer: Option<Direction>,
    auction_tokens: Vec<String>,
    play_leader: Option<Direction>,
    play_tokens: Vec<String>,
    // `OptimumResultTable` is a section too: the tag opens it and its twenty
    // rows follow, so it accumulates the same way.
    in_optimum: bool,
    optimum_rows: Vec<String>,
    /// Set once this board's table has been read from `OptimumResultTable`, so
    /// a `DoubleDummyTricks` tag later in the record does not overwrite it.
    /// The two encodings are redundant by design and only one of them has a
    /// specification to be checked against; see [`super::dd`].
    dd_from_optimum: bool,
    /// Name of the most recent tag in this record, so a `%` or `;` line can be
    /// anchored to the tag it follows.
    last_tag: Option<String>,
}

impl ParseState {
    fn in_auction(&self) -> bool {
        self.auction_dealer.is_some()
    }
    fn in_play(&self) -> bool {
        self.play_leader.is_some()
    }

    /// Finalize any open Auction/Play section into the current board.
    fn close_sections(&mut self) {
        if let Some(dealer) = self.auction_dealer.take() {
            if !self.auction_tokens.is_empty() {
                self.board.auction = Some(parse_auction(dealer, &self.auction_tokens));
            }
            self.auction_tokens.clear();
        }
        if let Some(leader) = self.play_leader.take() {
            if !self.play_tokens.is_empty() {
                let trump = self.board.contract.as_deref().and_then(contract_trump);
                self.board.play = Some(parse_play(leader, trump, &self.play_tokens));
            }
            self.play_tokens.clear();
        }
        if std::mem::take(&mut self.in_optimum) {
            let rows = std::mem::take(&mut self.optimum_rows);
            // A malformed section is dropped rather than half-decoded, for the
            // reason `dd_table_from_pbn` gives: a half-populated table carries
            // a producer/reader disagreement silently into whatever displays
            // it. Dropping it leaves any `DoubleDummyTricks` table already
            // read in place, which is the readable half of the pair.
            if let Ok(table) = super::optimum_result_table_from_rows(&rows) {
                self.board.double_dummy_tricks = Some(table);
                self.dd_from_optimum = true;
            }
        }
    }
}

/// Read boards from PBN content
///
/// `%` directives and `;` comments ride along on the board whose record they
/// sit in, anchored to the tag they follow, so [`write_pbn`](super::write_pbn)
/// can put them back where their author had them. Ones before the first record —
/// a Bridge Composer file header — are carried by the first board.
///
/// The one thing a `Vec<Board>` cannot carry is a file with *no* board records
/// at all, such as a header-only template: there is no board to hang its
/// directives on, and they are not returned. Use
/// [`PbnDocument`](super::PbnDocument) for that, and whenever the file's exact
/// bytes matter.
pub fn read_pbn(content: &str) -> Result<Vec<Board>> {
    let mut boards = Vec::new();
    let mut st = ParseState::default();

    for raw in content.lines() {
        let line = raw.trim();

        // Multi-line commentary block { ... } — capture text until closing brace.
        // Buffered raw: commentary is prose, and its indentation is the
        // author's. Trimming each line silently reflows what a renderer lays
        // out, which is a change to the page, not to whitespace.
        if st.in_commentary {
            st.commentary_buf.push(raw.to_string());
            if line.contains('}') {
                st.in_commentary = false;
                flush_commentary(&mut st);
            }
            continue;
        }

        // Blank line terminates the current board.
        if line.is_empty() {
            if st.has_content {
                st.close_sections();
                boards.push(std::mem::take(&mut st.board));
                st.has_content = false;
                st.last_tag = None;
                st.dd_from_optimum = false;
            }
            continue;
        }

        // Start of a commentary block.
        if line.starts_with('{') {
            st.commentary_buf.push(raw.to_string());
            if line.contains('}') {
                flush_commentary(&mut st);
            } else {
                st.in_commentary = true;
            }
            continue;
        }

        // File directives and line comments. Not board content, but content
        // all the same: `%` is where Bridge Composer keeps fonts, page setup
        // and colours. They ride along on the board whose record they sit in,
        // anchored to the tag they follow so a writer can put them back.
        if line.starts_with(';') || line.starts_with('%') {
            st.board.directives.push(Directive {
                text: line.to_string(),
                after_tag: st.last_tag.clone(),
            });
            continue;
        }

        // A tag pair closes any open section, then dispatches.
        if line.starts_with('[') {
            if let Some((tag, comment)) = parse_tag_line(line) {
                st.close_sections();
                st.has_content = true;
                st.last_tag = Some(tag.name.clone());
                apply_tag(&mut st, &tag);
                // A comment on the tag's own line refers to that tag, and rides
                // along on the same board-level list as one on a line of its own.
                if let Some(text) = comment {
                    st.board.directives.push(Directive {
                        text: text.to_string(),
                        after_tag: st.last_tag.clone(),
                    });
                }
            } else if let Some((tag, data)) = split_tag_and_data(line) {
                // The standard puts a section's data on the lines after its tag
                // pair, but producers exist that write the first datum on the
                // tag line itself — `[Play "W"]SJ`. Dropping the line loses the
                // tag *and* the datum, and for a Play section that datum is the
                // opening lead: the loss is total and silent.
                st.close_sections();
                st.has_content = true;
                st.last_tag = Some(tag.name.clone());
                apply_tag(&mut st, &tag);
                push_section_data(&mut st, data);
            }
            continue;
        }

        // Otherwise: a data line belonging to an open section.
        push_section_data(&mut st, line);
    }

    if st.has_content {
        st.close_sections();
        boards.push(st.board);
    }

    Ok(boards)
}

/// Push the buffered commentary block onto the board: what stood between the
/// block's opening `{` and its closing `}`, verbatim.
///
/// Only the braces come off. The text between them is the author's, down to the
/// line breaks and the spaces after them, and a consumer laying it out needs it
/// as written. Anything after the closing brace is not commentary and is
/// dropped, as is a block with nothing but whitespace in it.
fn flush_commentary(st: &mut ParseState) {
    let text = st.commentary_buf.join("\n");
    st.commentary_buf.clear();
    let Some(open) = text.find('{') else { return };
    let Some(close) = text.rfind('}') else { return };
    if close <= open {
        return;
    }
    let text = &text[open + 1..close];
    if !text.trim().is_empty() {
        st.board.commentary.push(text.to_string());
    }
}

/// Apply a parsed tag to the current board / open a section.
fn apply_tag(st: &mut ParseState, tag: &TagPair) {
    let board = &mut st.board;
    match tag.name.as_str() {
        "Board" => {
            board.number = tag.value.parse::<u32>().ok();
            if !tag.value.is_empty() {
                board.board_id = Some(tag.value.clone());
            }
        }
        "Dealer" => board.dealer = tag.value.chars().next().and_then(Direction::from_char),
        "Vulnerable" => board.vulnerable = Vulnerability::from_pbn(&tag.value).unwrap_or_default(),
        "Deal" => {
            if let Some(deal) = Deal::from_pbn(&tag.value) {
                board.deal = deal;
            }
        }
        "Event" => set_opt(&mut board.event, &tag.value),
        "Site" => set_opt(&mut board.site, &tag.value),
        "Date" => set_opt(&mut board.date, &tag.value),
        "Declarer" => {
            board.declarer = tag.value.chars().next().and_then(Direction::from_char);
        }
        "Contract" => {
            if !tag.value.is_empty() && tag.value != "?" {
                board.contract = Some(tag.value.clone());
            }
        }
        "Result" => board.result = tag.value.parse::<i8>().ok(),
        "North" | "East" | "South" | "West" => {
            if !tag.value.is_empty() {
                let dir = Direction::from_char(tag.name.chars().next().unwrap()).unwrap();
                board
                    .player_names
                    .get_or_insert_with(PlayerNames::new)
                    .set(dir, tag.value.clone());
            }
        }
        "Auction" => {
            st.auction_dealer = tag.value.chars().next().and_then(Direction::from_char);
        }
        "Play" => {
            st.play_leader = tag.value.chars().next().and_then(Direction::from_char);
        }
        "Note" => {
            // `[Note "n:text"]` annotates the auction just parsed.
            if let Some((num, text)) = tag.value.split_once(':') {
                if let (Ok(n), Some(auction)) = (num.trim().parse::<u8>(), board.auction.as_mut()) {
                    auction.add_note(n, text.to_string());
                }
            }
        }
        // A malformed value is dropped rather than half-decoded: see
        // `dd_table_from_pbn`. The tag is not preserved in `extra_tags` either,
        // since round-tripping a value we could not read would re-emit a
        // corruption as though it were analysis.
        "DoubleDummyTricks" => {
            // `OptimumResultTable` wins when a board carries both: it is the
            // encoding PBN 2.1 section 5.7 defines, and the two are redundant
            // by design. Sections close before the next tag is applied, so a
            // table read from either order of the pair is honoured.
            if !st.dd_from_optimum {
                board.double_dummy_tricks = super::dd_table_from_pbn(&tag.value).ok();
            }
        }
        // The rows follow; `close_sections` decodes them. Not preserved in
        // `extra_tags`: the header alone, with its rows consumed, would be
        // re-emitted as an empty section.
        "OptimumResultTable" => st.in_optimum = true,
        "OptimumScore" => board.optimum_score = Some(tag.value.clone()),
        "ParContract" => board.par_contract = Some(tag.value.clone()),
        // Everything else (standard-but-unmodeled + arbitrary custom tags) is
        // preserved verbatim rather than dropped.
        _ => board.extra_tags.push((tag.name.clone(), tag.value.clone())),
    }
}

fn set_opt(field: &mut Option<String>, value: &str) {
    if !value.is_empty() {
        *field = Some(value.to_string());
    }
}

/// Route a line of section data to whichever section is open.
fn push_section_data(st: &mut ParseState, line: &str) {
    if st.in_auction() {
        st.auction_tokens
            .extend(line.split_whitespace().map(str::to_string));
    } else if st.in_play() {
        st.play_tokens
            .extend(line.split_whitespace().map(str::to_string));
    } else if st.in_optimum {
        st.optimum_rows.push(line.to_string());
    }
}

/// Split a tag line that carries section data after its closing bracket, as in
/// `[Play "W"]SJ`. Returns the tag and the data, or `None` if the line is not
/// that shape — including a well-formed tag line, which has no data to give.
fn split_tag_and_data(line: &str) -> Option<(TagPair, &str)> {
    let open = line.find('"')?;
    let close = open + 1 + line[open + 1..].find('"')?;
    let bracket = close + 1 + line[close + 1..].find(']')?;
    let tag = parse_tag_pair(&line[..=bracket])?;
    let data = line[bracket + 1..].trim();
    (!data.is_empty()).then_some((tag, data))
}

/// Build an `Auction` from whitespace-split call tokens.
///
/// A call token may carry an annotation, either glued to the call (`1C!`,
/// `2H=1=`) or standing alone after it (`2H =1=`), and both forms mean the same
/// thing. The annotation is kept verbatim on the call — `"!"`, `"=1="`, `"$2"` —
/// rather than decoded, so a writer re-emits exactly what the file said and a
/// consumer that wants the note number reads it off the `=n=` form itself.
///
/// This matters more than it looks. Parsing the raw token as a call makes
/// `Call::from_pbn("1C!")` fail, and a dropped call shifts every later call one
/// seat: the auction still renders, and it is wrong. Lesson material is full of
/// these — 1,059 glued markers and 2,441 standalone note references across the
/// Baker Bridge and ABS collections.
fn parse_auction(dealer: Direction, tokens: &[String]) -> Auction {
    let mut auction = Auction::new(dealer);
    for tok in tokens {
        if let Some(end) = SectionEnd::from_pbn(tok) {
            auction.end = end;
            // `*` closes the section and nothing follows it. `+` is different:
            // the standard has it *replace the next call to be made* (3.5), so
            // it stands in a call's place and the annotations after it are that
            // placeholder's. Keeping it as a call is what gives them something
            // to attach to — and Bridge Composer, which writes `1D X Pass + $2`,
            // renders exactly that: the placeholder and its "?" annotation.
            if end == SectionEnd::Continued {
                auction.add_call(Call::Continue);
                continue;
            }
            break;
        }
        // "AP" — all pass. The three players yet to speak each pass, and the
        // auction is over. Not in the standard's call grammar, but Bridge
        // Composer and most lesson producers write it.
        if tok.eq_ignore_ascii_case("AP") {
            for _ in 0..3 {
                auction.add_call(Call::Pass);
            }
            break;
        }
        let (call_tok, annotation) = split_annotation(tok);
        if call_tok.is_empty() {
            // A standalone annotation belongs to the call before it.
            if let (Some(ann), Some(last)) = (annotation, auction.calls.last_mut()) {
                match last.annotation {
                    Some(ref mut existing) => existing.push_str(ann),
                    None => last.annotation = Some(ann.to_string()),
                }
            }
            continue;
        }
        if let Some(call) = Call::from_pbn(call_tok) {
            auction.add_annotated_call(call, annotation.map(str::to_string));
        }
    }
    auction
}

/// Split a call token into the call itself and any annotation trailing it.
///
/// Safe to split on the first `=`, `!`, `?` or `$` because no call token
/// contains one: the grammar is a level and a strain, `Pass`/`P`/`-`, `X`, `XX`,
/// `+`, or a run of underscores. A token that is *all* annotation (`=1=`, `$2`)
/// returns an empty call.
fn split_annotation(tok: &str) -> (&str, Option<&str>) {
    match tok.find(['=', '!', '?', '$']) {
        Some(at) => (&tok[..at], Some(&tok[at..])),
        None => (tok, None),
    }
}

/// Build a `PlaySequence` from whitespace-split card tokens, rotating the lead
/// to each trick's winner.
///
/// The four seats of a trick are filled in order, the opening leader first
/// (3.6), and `-` — a card unknown or not played — holds its seat like any
/// other token. Dropping the dashes instead slides every later card one seat
/// left, which does not merely lose information: a line like `- - - HJ`, whose
/// lead is *not* on record, comes back claiming ♥J was led. A handout then
/// prints a confident, wrong opening lead.
///
/// `+` likewise stands in for the card not yet played and holds its seat; the
/// standard notes it need not be the section's last token (`+ - - CQ`).
fn parse_play(leader: Direction, trump: Option<Suit>, tokens: &[String]) -> PlaySequence {
    let mut seq = PlaySequence::new(leader, trump);
    let mut seat = 0usize;

    for tok in tokens {
        if seq.tricks.is_empty() {
            seq.start_trick(leader);
        }
        // A full trick hands the lead to its winner — or, when the trick holds
        // unknowns and has no winner, back to whoever led it.
        if seat == 4 {
            let last = seq.tricks.last().expect("a trick is open");
            let next_leader = last.winner.unwrap_or(last.leader);
            seq.start_trick(next_leader);
            seat = 0;
        }

        if let Some(end) = SectionEnd::from_pbn(tok) {
            seq.end = end;
            if end == SectionEnd::Continued {
                seat += 1;
                continue;
            }
            break;
        }

        if tok == "-" {
            seat += 1;
            continue;
        }

        let Some(card) = parse_card(tok) else {
            continue;
        };
        let trick = seq.tricks.last_mut().expect("a trick is open");
        if seat == 0 {
            // Seat 0 through the trick's own API, which records the led suit.
            trick.play(card);
        } else {
            trick.cards[seat] = Some(card);
        }
        seat += 1;
        if trick.is_complete() {
            trick.determine_winner(trump);
        }
    }
    seq
}

/// Parse a PBN card token like `SA`, `HT`, `C2` into a `Card`.
fn parse_card(tok: &str) -> Option<Card> {
    let mut chars = tok.chars();
    let suit = Suit::from_char(chars.next()?)?;
    let rank = Rank::from_char(chars.next()?)?;
    Some(Card::new(suit, rank))
}

/// Trump suit implied by a contract string (`None` for NT or unparseable).
fn contract_trump(contract: &str) -> Option<Suit> {
    let strain = bridge_types::Contract::parse(contract)?.strain;
    match strain {
        Strain::Clubs => Some(Suit::Clubs),
        Strain::Diamonds => Some(Suit::Diamonds),
        Strain::Hearts => Some(Suit::Hearts),
        Strain::Spades => Some(Suit::Spades),
        Strain::NoTrump => None,
    }
}

/// Read boards from a PBN file
pub fn read_pbn_file(path: &std::path::Path) -> Result<Vec<Board>> {
    let content = std::fs::read_to_string(path)?;
    read_pbn(&content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_types::DdTable;

    use crate::pbn::{dd_table_from_pbn, optimum_result_table_header, optimum_result_table_rows};

    #[test]
    fn test_parse_tag_pair() {
        let tag = parse_tag_pair("[Board \"1\"]").unwrap();
        assert_eq!(tag.name, "Board");
        assert_eq!(tag.value, "1");

        let tag = parse_tag_pair("[Vulnerable \"NS\"]").unwrap();
        assert_eq!(tag.name, "Vulnerable");
        assert_eq!(tag.value, "NS");
    }

    #[test]
    fn test_read_simple_pbn() {
        let pbn = r#"
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
"#;
        let boards = read_pbn(pbn).unwrap();
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].number, Some(1));
        assert_eq!(boards[0].dealer, Some(Direction::North));
        assert_eq!(boards[0].vulnerable, Vulnerability::None);
    }

    #[test]
    fn test_read_multiple_boards() {
        let pbn = r#"
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]

[Board "2"]
[Dealer "E"]
[Vulnerable "NS"]
[Deal "E:Q7.AKT9.JT3.JT96 J653.QJ8.A.AQ732 K92.654.K954.K84 AT84.732.Q8762.5"]
"#;
        let boards = read_pbn(pbn).unwrap();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].number, Some(1));
        assert_eq!(boards[1].number, Some(2));
        assert_eq!(boards[1].dealer, Some(Direction::East));
        assert_eq!(boards[1].vulnerable, Vulnerability::NorthSouth);
    }

    #[test]
    fn test_read_pbn_with_commentary() {
        let pbn = r#"
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
{This is a multi-line
commentary that spans
several lines.}

[Board "2"]
[Dealer "E"]
[Vulnerable "NS"]
[Deal "E:Q7.AKT9.JT3.JT96 J653.QJ8.A.AQ732 K92.654.K954.K84 AT84.732.Q8762.5"]
"#;
        let boards = read_pbn(pbn).unwrap();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].commentary.len(), 1);
        assert!(boards[0].commentary[0].contains("multi-line"));
    }

    #[test]
    fn test_contract_declarer_result_and_custom_tags() {
        let pbn = r#"
[Board "7"]
[Dealer "S"]
[Vulnerable "None"]
[Deal "S:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
[Declarer "S"]
[Contract "3NT"]
[Result "9"]
[SkillPath "notrump/stayman"]
[Difficulty "2"]
"#;
        let boards = read_pbn(pbn).unwrap();
        let b = &boards[0];
        assert_eq!(b.contract.as_deref(), Some("3NT"));
        assert_eq!(b.declarer, Some(Direction::South));
        assert_eq!(b.result, Some(9));
        assert_eq!(b.extra_tag("SkillPath"), Some("notrump/stayman"));
        assert_eq!(b.extra_tag("Difficulty"), Some("2"));
        // Standard, dedicated-field tags must NOT leak into extra_tags.
        assert!(b
            .extra_tags
            .iter()
            .all(|(n, _)| n != "Contract" && n != "Declarer"));
    }

    #[test]
    fn test_non_integer_board_id_preserved() {
        let pbn = r#"
[Board "1-3"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
"#;
        let b = &read_pbn(pbn).unwrap()[0];
        assert_eq!(b.board_id.as_deref(), Some("1-3"));
        assert_eq!(b.number, None); // "1-3" is not a u32
    }

    #[test]
    fn test_auction_section() {
        let pbn = r#"
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
[Auction "N"]
1NT Pass 3NT Pass
Pass Pass
"#;
        let boards = read_pbn(pbn).unwrap();
        let auction = boards[0].auction.as_ref().expect("auction parsed");
        assert_eq!(auction.len(), 6);
        let fc = auction.final_contract().expect("final contract");
        assert_eq!(fc.level, 3);
        assert_eq!(fc.strain, Strain::NoTrump);
    }

    #[test]
    fn directives_and_comments_are_kept_and_anchored() {
        // The shape Bridge Composer and EPBot actually write: a per-board hash
        // between [Board] and the player names.
        let pbn = r#"% PBN 2.1
% Creator "Bridge Composer"

[Event "Club"]
[Board "1"]
% 065A62DCF61869AE5D72DF8D408A
; checked by hand
[North "EPBot"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
"#;
        let b = &read_pbn(pbn).unwrap()[0];
        // The file header preceded every tag, so it leads the first record.
        assert_eq!(
            b.leading_directives().collect::<Vec<_>>(),
            vec!["% PBN 2.1", "% Creator \"Bridge Composer\""]
        );
        assert_eq!(
            b.directives_after("Board").collect::<Vec<_>>(),
            vec!["% 065A62DCF61869AE5D72DF8D408A", "; checked by hand"]
        );
        assert!(b.directives_after("North").next().is_none());
        // They are directives, not tags: nothing leaked into extra_tags.
        assert!(b.extra_tags.is_empty());
    }

    #[test]
    fn a_directive_belongs_to_the_record_it_precedes() {
        let pbn = r#"[Board "1"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]

% between the boards

[Board "2"]
[Deal "E:Q7.AKT9.JT3.JT96 J653.QJ8.A.AQ732 K92.654.K954.K84 AT84.732.Q8762.5"]
"#;
        let boards = read_pbn(pbn).unwrap();
        assert_eq!(boards.len(), 2);
        assert!(boards[0].directives.is_empty());
        assert_eq!(
            boards[1].leading_directives().collect::<Vec<_>>(),
            vec!["% between the boards"]
        );
    }

    #[test]
    fn a_directive_does_not_start_a_board_or_end_one() {
        // Directives alone are not board content.
        assert!(read_pbn("% just a header\n; and a note\n")
            .unwrap()
            .is_empty());

        // And one inside an auction does not terminate the section.
        let pbn = r#"[Board "1"]
[Auction "N"]
1NT Pass 3NT Pass
% mid-auction note
Pass Pass
"#;
        let b = &read_pbn(pbn).unwrap()[0];
        assert_eq!(b.auction.as_ref().expect("auction parsed").len(), 6);
        assert_eq!(
            b.directives_after("Auction").collect::<Vec<_>>(),
            vec!["% mid-auction note"]
        );
    }

    #[test]
    fn commentary_braces_still_win_over_directives() {
        // A `%` inside {...} is commentary text, not a directive.
        let pbn = r#"[Board "1"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
{Declarer takes
% not a directive
all thirteen.}
"#;
        let b = &read_pbn(pbn).unwrap()[0];
        assert!(b.directives.is_empty());
        assert!(b.commentary[0].contains("% not a directive"));
    }

    #[test]
    fn a_file_with_no_boards_has_nothing_to_carry_its_directives() {
        // A Bridge Composer header template: all directives, no records. There
        // is no board to hang them on, so this is the one case `Vec<Board>`
        // cannot round-trip, and `PbnDocument` is the answer.
        let header = "%Content-type: text/x-pbn\n%BoardsPerPage 1\n%Margins 1000,1000\n";
        assert!(read_pbn(header).unwrap().is_empty());

        let doc = crate::pbn::PbnDocument::parse(header).unwrap();
        assert!(doc.boards().is_empty());
        assert_eq!(doc.to_pbn(), header);
    }

    #[test]
    fn a_comment_may_trail_a_tag_on_its_own_line() {
        let (tag, comment) = parse_tag_line("[Board \"1\"] ; the first board").unwrap();
        assert_eq!((tag.name.as_str(), tag.value.as_str()), ("Board", "1"));
        assert_eq!(comment, Some("; the first board"));

        // A brace comment that closes on the same line counts too.
        let (tag, comment) = parse_tag_line("[Result \"9\"] {made it}").unwrap();
        assert_eq!(tag.value, "9");
        assert_eq!(comment, Some("{made it}"));

        // No comment is still a tag.
        assert_eq!(parse_tag_line("[Board \"1\"]").unwrap().1, None);
    }

    #[test]
    fn a_trailing_comment_keeps_the_tag_and_is_anchored_to_it() {
        // Standard 3.8: a comment "refers to the preceding tag". Before, the
        // line did not end in `]`, so the tag was dropped along with it.
        let pbn = "[Board \"1\"] ; the first board\n[Result \"9\"]\n";
        let b = &read_pbn(pbn).unwrap()[0];
        assert_eq!(b.board_id.as_deref(), Some("1"));
        assert_eq!(b.result, Some(9));
        assert_eq!(
            b.directives_after("Board").collect::<Vec<_>>(),
            vec!["; the first board"]
        );
    }

    #[test]
    fn section_data_jammed_onto_a_tag_line_is_not_a_comment() {
        // Real files write `[Play "W"]S2`. Only a comment may follow a tag, so
        // calling that trailing text a comment would invent one — and then lose
        // it again on the next read, since a bare `S2` line is not anything.
        assert!(parse_tag_line("[Play \"W\"]S2").is_none());
        assert!(parse_tag_pair("[Play \"W\"]S2").is_none());
        // An unclosed brace comment belongs to the commentary scanner, not here.
        assert!(parse_tag_line("[Board \"1\"] {opens here").is_none());
    }

    #[test]
    fn a_section_keeps_the_marker_it_closed_with() {
        // `*` means no further cards will or can be given, so a play section
        // that is nothing but `*` still has something to say.
        let pbn = "[Board \"1\"]\n[Play \"W\"]\n*\n";
        let b = &read_pbn(pbn).unwrap()[0];
        let play = b.play.as_ref().expect("play section kept");
        assert_eq!(play.end, SectionEnd::Terminated);
        assert_eq!(play.opening_leader, Direction::West);

        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT Pass\n*\n";
        let a = read_pbn(pbn).unwrap()[0]
            .auction
            .clone()
            .expect("auction kept");
        assert_eq!(a.end, SectionEnd::Terminated);
        assert_eq!(a.len(), 2);

        // `+` says the next call is to be made another time.
        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT Pass\n+\n";
        let a = read_pbn(pbn).unwrap()[0]
            .auction
            .clone()
            .expect("auction kept");
        assert_eq!(a.end, SectionEnd::Continued);

        // An ordinary auction claims no marker.
        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT Pass Pass Pass\n";
        let a = read_pbn(pbn).unwrap()[0]
            .auction
            .clone()
            .expect("auction kept");
        assert_eq!(a.end, SectionEnd::Unmarked);
    }

    /// A board record carrying an `OptimumResultTable` section built from
    /// `table`, header and all, as a producer would write it.
    fn with_optimum_table(table: &DdTable, extra: &str) -> String {
        let mut text = format!(
            "[Board \"1\"]\n{extra}[OptimumResultTable \"{}\"]\n",
            optimum_result_table_header(table)
        );
        for row in optimum_result_table_rows(table) {
            text.push_str(&row);
            text.push('\n');
        }
        text
    }

    /// A table with no two cells alike, so an assertion about which encoding
    /// was read cannot pass by coincidence.
    fn counted_table() -> DdTable {
        let mut n = 0u8;
        DdTable::from_fn(|_, _| {
            n += 1;
            n % 14
        })
    }

    #[test]
    fn an_optimum_result_table_alone_fills_the_double_dummy_table() {
        // The standard encoding, PBN 2.1 section 5.7. Before, a board carrying
        // only this came back with no table at all and its analysis was lost.
        let table = counted_table();
        let b = &read_pbn(&with_optimum_table(&table, "")).unwrap()[0];
        assert_eq!(b.double_dummy_tricks.as_ref(), Some(&table));
        // The header is not left in `extra_tags`: its rows are consumed here,
        // and a writer would re-emit it as an empty section.
        assert!(b.extra_tags.iter().all(|(n, _)| n != "OptimumResultTable"));
    }

    #[test]
    fn optimum_result_table_wins_over_double_dummy_tricks() {
        // The two are redundant by design, so a file may carry both. Only
        // `OptimumResultTable` has a specification to be checked against, so it
        // is the one honoured — in either order, since a section closes before
        // the next tag is applied. The values disagree deliberately: every cell
        // of the tag says four tricks, and no cell of the table does.
        let table = counted_table();
        let tricks = "[DoubleDummyTricks \"44444444444444444444\"]\n";

        let before = read_pbn(&with_optimum_table(&table, tricks)).unwrap();
        assert_eq!(before[0].double_dummy_tricks.as_ref(), Some(&table));

        let after = read_pbn(&format!("{}{tricks}", with_optimum_table(&table, ""))).unwrap();
        assert_eq!(after[0].double_dummy_tricks.as_ref(), Some(&table));
    }

    #[test]
    fn optimum_rows_may_arrive_in_any_order() {
        // Each row names its own declarer and denomination, so the order they
        // are written in carries nothing.
        let table = counted_table();
        let written = with_optimum_table(&table, "");
        let mut lines: Vec<&str> = written.lines().collect();
        let rows = lines.split_off(2);
        lines.extend(rows.into_iter().rev());

        let b = &read_pbn(&format!("{}\n", lines.join("\n"))).unwrap()[0];
        assert_eq!(b.double_dummy_tricks.as_ref(), Some(&table));
    }

    #[test]
    fn the_two_encodings_agree_on_a_real_bridge_composer_file() {
        // Every board of Bridge Composer's own output carries both tags, and
        // its help says its Double Dummy commands keep them in step. Since the
        // section now wins, this compares the table decoded from the standard
        // rows against the compact tag the same board was written with — the
        // two decoders' cell orders checked against each other on real data.
        let text = include_str!("../../fixtures/bridge-composer/pbn-order-test-bc.pbn");
        let doc = crate::pbn::PbnDocument::parse(text).unwrap();
        let mut compared = 0;
        for (index, board) in doc.boards().iter().enumerate() {
            let Some(value) = doc.tag(index, "DoubleDummyTricks") else {
                continue;
            };
            let from_tag = dd_table_from_pbn(value).unwrap();
            assert_eq!(
                board.double_dummy_tricks.as_ref(),
                Some(&from_tag),
                "board {index}: the section and the tag disagree"
            );
            compared += 1;
        }
        assert_eq!(compared, 8, "a template board, then the eight");
    }

    #[test]
    fn a_short_optimum_result_table_is_dropped_not_half_decoded() {
        // Nineteen of the twenty cells. A half-populated table would carry a
        // producer/reader disagreement silently into whatever displayed it, so
        // the section goes; the readable `DoubleDummyTricks` tag still stands.
        let table = counted_table();
        let short: String = with_optimum_table(&table, "")
            .lines()
            .take(21)
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(read_pbn(&short).unwrap()[0].double_dummy_tricks.is_none());

        let tricks = "[DoubleDummyTricks \"44444444444444444444\"]\n";
        let b = &read_pbn(&format!("{short}{tricks}")).unwrap()[0];
        assert_eq!(
            b.double_dummy_tricks.as_ref(),
            Some(&dd_table_from_pbn("44444444444444444444").unwrap())
        );
    }

    #[test]
    fn an_annotated_call_survives_with_its_annotation() {
        // The failure this guards against is not a lost annotation but a lost
        // *call*: parsing "1C!" as a call fails, and dropping it moves every
        // later call one seat.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"N\"]\n1C! 1H 2C$1 Pass\n").unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.len(), 4);
        assert_eq!(a.calls[0].call, Call::bid(1, Strain::Clubs));
        assert_eq!(a.calls[0].annotation.as_deref(), Some("!"));
        assert_eq!(a.calls[1].call, Call::bid(1, Strain::Hearts));
        assert_eq!(a.calls[1].annotation, None);
        assert_eq!(a.calls[2].call, Call::bid(2, Strain::Clubs));
        assert_eq!(a.calls[2].annotation.as_deref(), Some("$1"));
        assert_eq!(a.calls[3].call, Call::Pass);
    }

    #[test]
    fn a_standalone_note_reference_annotates_the_call_before_it() {
        // Both spellings occur, and they mean the same thing. The standalone
        // form is the common one: 2,441 of them across the lesson collections.
        let glued = read_pbn("[Board \"1\"]\n[Auction \"E\"]\n2NT=1= Pass\n").unwrap();
        let spaced = read_pbn("[Board \"1\"]\n[Auction \"E\"]\n2NT =1= Pass\n").unwrap();
        for boards in [glued, spaced] {
            let a = boards[0].auction.as_ref().unwrap();
            assert_eq!(a.len(), 2, "the reference is not a call");
            assert_eq!(a.calls[0].annotation.as_deref(), Some("=1="));
            assert_eq!(a.calls[1].call, Call::Pass);
        }
    }

    #[test]
    fn a_note_reference_resolves_against_the_note_tag() {
        let boards =
            read_pbn("[Board \"1\"]\n[Auction \"N\"]\n1NT =1= Pass\n[Note \"1:15-17 balanced\"]\n")
                .unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.calls[0].annotation.as_deref(), Some("=1="));
        assert_eq!(a.get_note(1), Some("15-17 balanced"));
    }

    #[test]
    fn ap_is_the_three_passes_it_stands_for() {
        let boards = read_pbn("[Board \"1\"]\n[Auction \"N\"]\n1NT Pass 3NT AP\n").unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.len(), 6);
        assert!(a.calls[3..].iter().all(|c| c.call == Call::Pass));
        // The auction is over: nothing after AP is read.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"N\"]\nPass AP 1NT\n").unwrap();
        assert_eq!(boards[0].auction.as_ref().unwrap().len(), 4);
    }

    #[test]
    fn a_continue_marker_stands_in_for_the_call_not_yet_made() {
        // `+` replaces the next call (3.5), so it holds a call's place and the
        // NAG after it is that placeholder's annotation. Verified against
        // Bridge Composer 5.118.2, which writes `1D X Pass + $2` and renders
        // the South cell as "??" — the placeholder and its "?" annotation.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"W\"]\n1D X Pass + $2\n").unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.len(), 4);
        assert_eq!(a.calls[3].call, Call::Continue);
        assert_eq!(a.calls[3].annotation.as_deref(), Some("$2"));
        assert_eq!(a.end, SectionEnd::Continued);

        // A bare `+` is the same placeholder with nothing to say about it.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"W\"]\n1D X Pass +\n").unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.len(), 4);
        assert_eq!(a.calls[3].annotation, None);

        // `*` is the other marker, and it is not a call.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"W\"]\n1D X Pass *\n").unwrap();
        let a = boards[0].auction.as_ref().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a.end, SectionEnd::Terminated);
    }

    #[test]
    fn a_placeholder_is_written_back_once() {
        // The `+` comes from the call; appending the end marker as well would
        // write it twice.
        use crate::pbn::write_pbn;
        let pbn = "[Board \"1\"]\n[Auction \"W\"]\n1D X Pass + $2\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(out.contains("1D X Pass +$2"), "in:\n{out}");
        assert!(
            !out.contains("+$2\n+"),
            "the marker is not written twice:\n{out}"
        );
        assert_eq!(
            write_pbn(&read_pbn(&out).unwrap()),
            out,
            "stable on re-read"
        );
    }

    #[test]
    fn an_unreadable_token_is_still_skipped() {
        // Splitting on the annotation characters must not turn junk into calls.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"N\"]\n1NT wat! Pass\n").unwrap();
        assert_eq!(boards[0].auction.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn section_data_on_the_tag_line_is_taken_not_dropped() {
        // `[Play "W"]SJ`: the opening lead jammed onto the tag. Refusing the
        // line loses the tag as well as the datum, which is how a whole
        // collection's opening leads went missing without a word.
        let boards = read_pbn("[Board \"1\"]\n[Play \"W\"]SJ\nH2 D3 C4 S2\n").unwrap();
        let play = boards[0].play.as_ref().expect("a play section");
        assert_eq!(play.opening_leader, Direction::West);
        assert_eq!(
            play.tricks[0].cards[0],
            Some(Card::new(Suit::Spades, Rank::Jack)),
            "the lead is the datum on the tag line"
        );

        // The same for an auction.
        let boards = read_pbn("[Board \"1\"]\n[Auction \"N\"]1NT\nPass 3NT Pass\n").unwrap();
        let auction = boards[0].auction.as_ref().expect("an auction");
        assert_eq!(auction.len(), 4);
        assert_eq!(auction.calls[0].call, Call::bid(1, Strain::NoTrump));

        // A well-formed tag line is untouched by this path.
        let boards = read_pbn("[Board \"1\"]\n[Play \"W\"]\nSJ\n").unwrap();
        assert_eq!(
            boards[0].play.as_ref().unwrap().tricks[0].cards[0],
            Some(Card::new(Suit::Spades, Rank::Jack))
        );
    }

    #[test]
    fn a_dash_holds_its_seat_in_a_trick() {
        // `- - - HJ` with East on lead: the lead is not on record, and ♥J is
        // North's card, the fourth of the trick. Skipping the dashes would
        // report ♥J as the opening lead — an invented fact, not a lost one.
        let boards = read_pbn("[Board \"1\"]\n[Play \"E\"]\n- - - HJ\n").unwrap();
        let play = boards[0].play.as_ref().unwrap();
        let trick = &play.tricks[0];
        assert_eq!(trick.cards[0], None, "the opening lead is unknown");
        assert_eq!(trick.cards[3], Some(Card::new(Suit::Hearts, Rank::Jack)));
        assert_eq!(
            trick.card_by(Direction::North),
            Some(Card::new(Suit::Hearts, Rank::Jack))
        );
        assert_eq!(trick.lead_suit, None);
    }

    #[test]
    fn a_placeholder_holds_its_seat_too() {
        // The standard's own example: `+` is West's, and the cards after it
        // keep their seats (3.6).
        let boards = read_pbn("[Board \"1\"]\n[Play \"W\"]\nH2 H3 H4 HA\n+ - - CQ\n").unwrap();
        let play = boards[0].play.as_ref().unwrap();
        assert_eq!(play.tricks.len(), 2);
        let second = &play.tricks[1];
        assert_eq!(second.cards[0], None, "West has not played yet");
        assert_eq!(second.cards[3], Some(Card::new(Suit::Clubs, Rank::Queen)));
        assert_eq!(play.end, SectionEnd::Continued);
    }
}
