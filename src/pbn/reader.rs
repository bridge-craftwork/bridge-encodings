//! PBN file reader.
//!
//! Section-aware: besides the scalar tags, it parses the `Auction` and `Play`
//! sections into typed `Auction`/`PlaySequence`, captures `{...}` commentary
//! blocks, and preserves every tag it does not otherwise model as an
//! `extra_tags` pair on the board (the PBN spec permits arbitrary supplemental
//! tags; dropping them is lossy). `%` directives and `;` comments are kept too,
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
        if st.in_commentary {
            st.commentary_buf.push(line.to_string());
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
            }
            continue;
        }

        // Start of a commentary block.
        if line.starts_with('{') {
            st.commentary_buf.push(line.to_string());
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
            }
            continue;
        }

        // Otherwise: a data line belonging to an open section.
        if st.in_auction() {
            st.auction_tokens
                .extend(line.split_whitespace().map(str::to_string));
        } else if st.in_play() {
            st.play_tokens
                .extend(line.split_whitespace().map(str::to_string));
        }
    }

    if st.has_content {
        st.close_sections();
        boards.push(st.board);
    }

    Ok(boards)
}

/// Push the buffered commentary block (braces/whitespace stripped) onto the board.
fn flush_commentary(st: &mut ParseState) {
    let text = st.commentary_buf.join("\n");
    st.commentary_buf.clear();
    let text = text
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();
    if !text.is_empty() {
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
        "DoubleDummyTricks" => board.double_dummy_tricks = Some(tag.value.clone()),
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

/// Build an `Auction` from whitespace-split call tokens. Note-reference tokens
/// (`=n=`) and section markers (`*`) are skipped; unrecognized tokens are
/// ignored so a stray annotation never corrupts the call sequence.
fn parse_auction(dealer: Direction, tokens: &[String]) -> Auction {
    let mut auction = Auction::new(dealer);
    for tok in tokens {
        if let Some(end) = SectionEnd::from_pbn(tok) {
            auction.end = end;
            break;
        }
        if tok.starts_with('=') {
            continue;
        }
        if let Some(call) = Call::from_pbn(tok) {
            auction.add_call(call);
        }
    }
    auction
}

/// Build a `PlaySequence` from whitespace-split card tokens, rotating the lead
/// to each trick's winner. Best-effort: unknown cards (`-`) are skipped, so a
/// redacted play may not reconstruct exact trick boundaries.
fn parse_play(leader: Direction, trump: Option<Suit>, tokens: &[String]) -> PlaySequence {
    let mut seq = PlaySequence::new(leader, trump);
    for tok in tokens {
        if let Some(end) = SectionEnd::from_pbn(tok) {
            seq.end = end;
            break;
        }
        let Some(card) = parse_card(tok) else {
            continue;
        };
        // Start a fresh trick, led by the previous winner, once one completes.
        if let Some(last) = seq.tricks.last() {
            if last.is_complete() {
                let next_leader = last.winner.unwrap_or(leader);
                seq.start_trick(next_leader);
            }
        }
        seq.play_card(card);
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
}
