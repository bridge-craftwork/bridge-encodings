//! PBN file reader.
//!
//! Section-aware: besides the scalar tags, it parses the `Auction` and `Play`
//! sections into typed `Auction`/`PlaySequence`, captures `{...}` commentary
//! blocks, and preserves every tag it does not otherwise model as an
//! `extra_tags` pair on the board (the PBN spec permits arbitrary supplemental
//! tags; dropping them is lossy). Board records are terminated by a blank line,
//! per the PBN standard.

use crate::error::Result;
use bridge_types::{
    Auction, Board, Call, Card, Deal, Direction, PlaySequence, PlayerNames, Rank, Strain, Suit,
    Vulnerability,
};

/// A parsed PBN tag pair
#[derive(Debug, Clone)]
pub struct TagPair {
    pub name: String,
    pub value: String,
}

/// Parse a tag pair from a line: [TagName "value"]
fn parse_tag_pair(line: &str) -> Option<TagPair> {
    let line = line.trim();
    if !line.starts_with('[') || !line.ends_with(']') {
        return None;
    }

    let inner = &line[1..line.len() - 1];

    // Find the space between tag name and quoted value
    let space_pos = inner.find(' ')?;
    let name = inner[..space_pos].trim().to_string();
    let rest = inner[space_pos..].trim();

    // Extract quoted value
    if !rest.starts_with('"') || !rest.ends_with('"') {
        return None;
    }
    let value = rest[1..rest.len() - 1].to_string();

    Some(TagPair { name, value })
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
                let trump = self
                    .board
                    .contract
                    .as_deref()
                    .and_then(contract_trump);
                self.board.play = Some(parse_play(leader, trump, &self.play_tokens));
            }
            self.play_tokens.clear();
        }
    }
}

/// Read boards from PBN content
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

        // File directives / line comments — not board content.
        if line.starts_with(';') || line.starts_with('%') {
            continue;
        }

        // A tag pair closes any open section, then dispatches.
        if line.starts_with('[') {
            if let Some(tag) = parse_tag_pair(line) {
                st.close_sections();
                st.has_content = true;
                apply_tag(&mut st, &tag);
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
    let text = text.trim().trim_start_matches('{').trim_end_matches('}').trim();
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
        if tok.starts_with('=') || *tok == "*" {
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
        if *tok == "*" {
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
        assert!(b.extra_tags.iter().all(|(n, _)| n != "Contract" && n != "Declarer"));
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
}
