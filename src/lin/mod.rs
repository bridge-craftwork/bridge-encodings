//! LIN, the pipe-delimited format Bridge Base Online uses for hand records,
//! handviewer links and teaching movies.
//!
//! A LIN text is a run of `command|value|` pairs. This module works with it at
//! two levels:
//!
//! - [`LinDocument`] keeps a text as its tokens and writes it back byte for
//!   byte, whatever commands it uses. A teaching movie, with its rewinds,
//!   overlays and commentary, survives a read/write cycle untouched.
//! - [`LinData`] is the board a record describes: names, deal, vulnerability,
//!   auction, play and claim. [`parse_lin`] and [`parse_lin_file`] read it,
//!   [`write_lin`] and [`write_lin_file`] write it, and
//!   [`LinData::from_board`] builds one from a board read from PBN.
//!
//! Bridge Composer 5.118.2 is the reference for both directions. What it does
//! is recorded in `fixtures/lin/README.md`, and the tests check against its
//! output.

mod document;
mod writer;

pub use document::{LinDocument, LinToken};
pub use writer::{write_lin, write_lin_file};

use crate::error::Result;
use bridge_types::{Card, Deal, Direction, Hand, Rank, Suit, Vulnerability};

/// A bid with optional alert and annotation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidWithAnnotation {
    /// The bid string (e.g., "1C", "p", "d", "r", "1N")
    pub bid: String,
    /// Whether the bid was alerted
    pub alert: bool,
    /// Optional annotation/explanation
    pub annotation: Option<String>,
}

/// Parsed LIN data from a BBO hand record
#[derive(Debug, Clone)]
pub struct LinData {
    /// Player names in S, W, N, E order (BBO convention)
    pub player_names: [String; 4],
    /// Dealer position
    pub dealer: Direction,
    /// The deal. A hand LIN leaves unknown is empty.
    pub deal: Deal,
    /// Vulnerability
    pub vulnerability: Vulnerability,
    /// The record's title (`mn`), e.g. "Club game - 2026.09.14"
    pub title: Option<String>,
    /// The room and board (`qx`), e.g. "o1,BOARD 1": `o` for the open room,
    /// `c` for the closed
    pub room_board: Option<String>,
    /// Board header (e.g., "Board 1")
    pub board_header: Option<String>,
    /// The auction sequence
    pub auction: Vec<BidWithAnnotation>,
    /// All cards played in order
    pub play: Vec<Card>,
    /// Claim (number of tricks), if hand was claimed
    pub claim: Option<u8>,
}

/// Two records are equal when they describe the same board. A hand compares by
/// the cards it holds, not the order they were listed in, since LIN computes
/// the fourth hand rather than reading it.
impl PartialEq for LinData {
    fn eq(&self, other: &Self) -> bool {
        self.player_names == other.player_names
            && self.dealer == other.dealer
            && Direction::ALL
                .iter()
                .all(|d| same_cards(self.deal.hand(*d), other.deal.hand(*d)))
            && self.vulnerability == other.vulnerability
            && self.title == other.title
            && self.room_board == other.room_board
            && self.board_header == other.board_header
            && self.auction == other.auction
            && self.play == other.play
            && self.claim == other.claim
    }
}

fn same_cards(a: &Hand, b: &Hand) -> bool {
    a.len() == b.len() && a.cards().iter().all(|c| b.has_card(*c))
}

impl LinData {
    /// Format the cardplay as a trick-by-trick string
    /// Output format: "D2 DA D6 D5|S3 S2 SQ SA|..."
    pub fn format_cardplay_by_trick(&self) -> String {
        if self.play.is_empty() {
            return String::new();
        }

        let tricks: Vec<String> = self
            .play
            .chunks(4)
            .map(|trick| {
                trick
                    .iter()
                    .map(|card| format!("{}{}", card.suit.to_char(), card.rank.to_char()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();

        tricks.join("|")
    }
}

/// Parse one LIN record into [`LinData`].
///
/// Values are kept as written. In particular `+` is not read as a space: an
/// explanation such as "5+ hearts" means what it says, and Bridge Composer
/// reads `ah|Board+7|` as the header `Board+7`, not board 7. A caller that
/// took the text from a URL decodes it first.
///
/// Commands the board does not use (commentary, page breaks, highlights, a
/// movie's rewinds) are skipped; [`LinDocument`] keeps them.
pub fn parse_lin(lin_str: &str) -> Result<LinData> {
    Ok(from_tokens(&LinDocument::parse(lin_str).tokens))
}

/// Build the board a run of tokens describes.
fn from_tokens(tokens: &[LinToken]) -> LinData {
    let mut player_names = [String::new(), String::new(), String::new(), String::new()];
    let mut dealer = Direction::North;
    let mut deal = Deal::new();
    let mut vulnerability = Vulnerability::None;
    let mut title = None;
    let mut room_board = None;
    let mut board_header = None;
    let mut auction: Vec<BidWithAnnotation> = Vec::new();
    let mut play = Vec::new();
    let mut claim = None;

    for token in tokens {
        let value = token.value.as_str();
        match token.command().to_ascii_lowercase().as_str() {
            "pn" => {
                for (slot, name) in player_names.iter_mut().zip(value.split(',')) {
                    *slot = name.to_string();
                }
            }
            // A movie's later `md` records start with `0`, not a dealer digit:
            // they re-deal the defenders' cards for a variation, and are not
            // this board's deal.
            "md" => {
                if let Some((d, hands)) = parse_md(value.trim()) {
                    dealer = d;
                    deal = hands;
                }
            }
            "sv" => vulnerability = parse_sv(value.trim()),
            "mn" => title = Some(value.to_string()).filter(|v| !v.is_empty()),
            "qx" => room_board = Some(value.to_string()).filter(|v| !v.is_empty()),
            "ah" => board_header = Some(value.to_string()),
            "mb" => {
                let alert = value.ends_with('!');
                auction.push(BidWithAnnotation {
                    bid: value.trim_end_matches('!').to_string(),
                    alert,
                    annotation: None,
                });
            }
            "an" => {
                if let Some(last_bid) = auction.last_mut() {
                    last_bid.annotation = Some(value.to_string());
                }
            }
            "pc" => {
                if let Some(card) = parse_card(value.trim()) {
                    play.push(card);
                }
            }
            "mc" => claim = value.trim().parse().ok(),
            _ => {}
        }
    }

    LinData {
        player_names,
        dealer,
        deal,
        vulnerability,
        title,
        room_board,
        board_header,
        auction,
        play,
        claim,
    }
}

/// Parse the md (make deal) field
///
/// Format: dealer digit, then hands in S, W, N, E order separated by commas.
/// A hand left empty is unknown. East is usually omitted, and is computed only
/// when the other three are all complete — Bridge Composer leaves the rest
/// unknown when they are not, so `md|1S…,,,|` is South's hand and nothing else.
fn parse_md(md_str: &str) -> Option<(Direction, Deal)> {
    // First character is dealer: 1=S, 2=W, 3=N, 4=E (BBO convention)
    let dealer = match md_str.chars().next()? {
        '1' => Direction::South,
        '2' => Direction::West,
        '3' => Direction::North,
        '4' => Direction::East,
        _ => return None,
    };

    let fields: Vec<&str> = md_str[1..].split(',').collect();
    let mut deal = Deal::new();
    for (seat, field) in LIN_SEATS.iter().zip(&fields) {
        deal.set_hand(*seat, parse_lin_hand(field));
    }

    let east_given = fields.get(3).is_some_and(|f| !f.trim().is_empty());
    if !east_given && LIN_SEATS[..3].iter().all(|s| deal.hand(*s).len() == 13) {
        deal.set_hand(
            Direction::East,
            calculate_fourth_hand(&deal, Direction::East),
        );
    }

    Some((dealer, deal))
}

/// The seats in the order LIN lists them: `pn` names and `md` hands.
const LIN_SEATS: [Direction; 4] = [
    Direction::South,
    Direction::West,
    Direction::North,
    Direction::East,
];

/// Parse a single hand in LIN format
/// Format: suits concatenated with suit letter prefix (SHDC order)
fn parse_lin_hand(hand_str: &str) -> Hand {
    let mut hand = Hand::new();
    let mut current_suit: Option<Suit> = None;

    for c in hand_str.chars() {
        match c.to_ascii_uppercase() {
            'S' => current_suit = Some(Suit::Spades),
            'H' => current_suit = Some(Suit::Hearts),
            'D' => current_suit = Some(Suit::Diamonds),
            'C' => current_suit = Some(Suit::Clubs),
            _ => {
                if let Some(suit) = current_suit {
                    if let Some(rank) = Rank::from_char(c) {
                        hand.add_card(Card::new(suit, rank));
                    }
                }
            }
        }
    }

    hand
}

/// Calculate the fourth hand from the three known hands
fn calculate_fourth_hand(deal: &Deal, fourth_dir: Direction) -> Hand {
    let mut fourth = Hand::new();

    for suit in Suit::ALL {
        for rank in Rank::ALL {
            let card = Card::new(suit, rank);
            let held = Direction::ALL
                .iter()
                .any(|dir| *dir != fourth_dir && deal.hand(*dir).has_card(card));
            if !held {
                fourth.add_card(card);
            }
        }
    }

    fourth
}

/// Parse vulnerability from sv field
fn parse_sv(sv: &str) -> Vulnerability {
    match sv.to_lowercase().as_str() {
        "o" | "0" | "-" => Vulnerability::None,
        "n" | "ns" => Vulnerability::NorthSouth,
        "e" | "ew" => Vulnerability::EastWest,
        "b" | "both" | "all" => Vulnerability::Both,
        _ => Vulnerability::None,
    }
}

/// Parse a card from LIN format (e.g., "D2", "SA", "HK")
///
/// Exactly a suit and a rank. A movie also plays `pc|s|` (some spade), `pc|!sa|`
/// and longer forms; none of them is a card of the board's play.
fn parse_card(card_str: &str) -> Option<Card> {
    let mut chars = card_str.chars();
    let suit = Suit::from_char(chars.next()?)?;
    let rank = Rank::from_char(chars.next()?)?;
    if chars.next().is_some() {
        return None;
    }

    Some(Card::new(suit, rank))
}

/// Parse every board in a LIN file.
///
/// A file with `qx` records — what Bridge Composer and BBO tournament exports
/// write, and [`write_lin_file`] too — is split at each board's `qx`, taking
/// the `mn` and `pn` records just before it along; a board there spans several
/// lines. Anything before the first board that holds no deal, such as a
/// tournament file's `vg`/`rs`/`pw`/`mp`/`bn` header, is not a board.
///
/// A file without `qx` holds one board per line.
pub fn parse_lin_file(content: &str) -> Result<Vec<LinData>> {
    let doc = LinDocument::parse(content);
    if doc.tokens.iter().any(|t| t.is("qx")) {
        return Ok(split_at_boards(&doc.tokens)
            .into_iter()
            .filter(|board| board.iter().any(|t| t.is("md")))
            .map(from_tokens)
            .collect());
    }

    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| from_tokens(&LinDocument::parse(line).tokens))
        .collect())
}

/// Split tokens into boards, each starting at a `qx` or the `mn`/`pn` records
/// directly before it.
fn split_at_boards(tokens: &[LinToken]) -> Vec<&[LinToken]> {
    let mut starts = vec![0];
    for (i, token) in tokens.iter().enumerate() {
        if !token.is("qx") {
            continue;
        }
        let mut start = i;
        while start > 0 && (tokens[start - 1].is("mn") || tokens[start - 1].is("pn")) {
            start -= 1;
        }
        if start > starts.last().copied().unwrap_or(0) {
            starts.push(start);
        }
    }

    let mut boards = Vec::with_capacity(starts.len());
    for (n, start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(tokens.len());
        boards.push(&tokens[*start..end]);
    }
    boards
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_card() {
        let card = parse_card("SA").unwrap();
        assert_eq!(card.suit, Suit::Spades);
        assert_eq!(card.rank, Rank::Ace);

        let card = parse_card("D2").unwrap();
        assert_eq!(card.suit, Suit::Diamonds);
        assert_eq!(card.rank, Rank::Two);

        let card = parse_card("HT").unwrap();
        assert_eq!(card.suit, Suit::Hearts);
        assert_eq!(card.rank, Rank::Ten);
    }

    #[test]
    fn a_movie_play_that_is_not_one_card_is_not_a_card() {
        for movie_only in ["s", "!sa", "!s", "s2hdc", ""] {
            assert_eq!(parse_card(movie_only), None, "{movie_only:?}");
        }
    }

    #[test]
    fn test_parse_sv() {
        assert_eq!(parse_sv("o"), Vulnerability::None);
        assert_eq!(parse_sv("b"), Vulnerability::Both);
        assert_eq!(parse_sv("n"), Vulnerability::NorthSouth);
        assert_eq!(parse_sv("e"), Vulnerability::EastWest);
    }

    #[test]
    fn test_parse_lin_hand() {
        let hand = parse_lin_hand("SAKQHJT9D8765C432");
        assert_eq!(hand.suit_length(Suit::Spades), 3);
        assert_eq!(hand.suit_length(Suit::Hearts), 3);
        assert_eq!(hand.suit_length(Suit::Diamonds), 4);
        assert_eq!(hand.suit_length(Suit::Clubs), 3);
    }

    #[test]
    fn test_parse_lin_basic() {
        let lin = "pn|South,West,North,East|md|3SAKHJD876C5432,S2HQT9DKQ5CKQJT9,SQJT9HA32DAJ2CA8,|sv|o|ah|Board 1|mb|1C|mb|p|pc|D2|pc|DA|pc|D3|pc|D8|";

        let data = parse_lin(lin).unwrap();
        assert_eq!(data.player_names[0], "South");
        assert_eq!(data.player_names[2], "North");
        assert_eq!(data.dealer, Direction::North);
        assert_eq!(data.vulnerability, Vulnerability::None);
        assert_eq!(data.board_header, Some("Board 1".to_string()));
        assert_eq!(data.auction.len(), 2);
        assert_eq!(data.auction[0].bid, "1C");
        assert_eq!(data.play.len(), 4);
    }

    #[test]
    fn test_format_cardplay_by_trick() {
        let lin = "pn|S,W,N,E|md|3SAKHJD876C5432,S2HQT9DKQ5CKQJT9,SQJT9HA32DAJ2CA8,|sv|o|pc|D2|pc|DA|pc|D3|pc|D8|pc|H2|pc|H4|pc|HJ|pc|HQ|";
        let data = parse_lin(lin).unwrap();
        let cardplay = data.format_cardplay_by_trick();
        assert_eq!(cardplay, "D2 DA D3 D8|H2 H4 HJ HQ");
    }

    #[test]
    fn test_parse_lin_with_alerts() {
        let lin = "pn|S,W,N,E|md|1SAKHJD876C5432,,,|sv|b|mb|1C!|an|could be short|mb|p|mb|1H!|an|5+ hearts|";
        let data = parse_lin(lin).unwrap();

        assert_eq!(data.auction.len(), 3);
        assert!(data.auction[0].alert);
        assert_eq!(
            data.auction[0].annotation,
            Some("could be short".to_string())
        );
        assert!(!data.auction[1].alert);
        assert!(data.auction[2].alert);
        assert_eq!(data.auction[2].annotation, Some("5+ hearts".to_string()));
    }

    // What Bridge Composer 5.118.2 reads from each `read-*.lin` is its `-bc.pbn`
    // beside it; see fixtures/lin/README.md.

    fn hand_lengths(data: &LinData) -> [usize; 4] {
        LIN_SEATS.map(|seat| data.deal.hand(seat).len())
    }

    #[test]
    fn plus_is_not_a_space() {
        // BC: [West "West+Player"], [Board ""], [Note "2:Four+hearts"].
        let data = parse_lin(include_str!(
            "../../fixtures/lin/read-plus-alerts-claim.lin"
        ))
        .unwrap();
        assert_eq!(data.player_names[1], "West+Player");
        assert_eq!(data.player_names[3], "Ed East");
        assert_eq!(data.board_header.as_deref(), Some("Board+7"));
        assert_eq!(data.auction[3].annotation.as_deref(), Some("Four+hearts"));
        assert_eq!(
            data.auction[1].annotation.as_deref(),
            Some("5+ clubs, or 4+ if 4-4-3-2")
        );
    }

    #[test]
    fn percent_escapes_are_not_decoded() {
        // BC: [South "a%20b"], [Board ""], [Note "1:15%2B HCP"].
        let data = parse_lin(include_str!("../../fixtures/lin/read-percent-encoding.lin")).unwrap();
        assert_eq!(data.player_names[0], "a%20b");
        assert_eq!(data.board_header.as_deref(), Some("Board%2010"));
        assert_eq!(data.auction[0].annotation.as_deref(), Some("15%2B HCP"));
    }

    #[test]
    fn three_hands_give_the_fourth_and_a_claim() {
        // BC: a full deal, [Result "9"].
        let data = parse_lin(include_str!(
            "../../fixtures/lin/read-plus-alerts-claim.lin"
        ))
        .unwrap();
        assert_eq!(hand_lengths(&data), [13; 4]);
        assert!(data.deal.is_valid());
        assert_eq!(data.claim, Some(9));
        assert_eq!(data.play.len(), 8);
    }

    #[test]
    fn a_fourth_hand_that_is_written_is_read() {
        // BC: [Deal "S:962.AJ7.KT82.J75 T5.Q9863.A943.KQ K843.T542.J6.863 AQJ7.K.Q75.AT942"].
        let data = parse_lin(include_str!("../../fixtures/lin/read-four-hands.lin")).unwrap();
        assert_eq!(hand_lengths(&data), [13; 4]);
        assert!(data.deal.is_valid());
        let bids: Vec<&str> = data.auction.iter().map(|b| b.bid.as_str()).collect();
        assert_eq!(bids, ["1NT", "P", "2NT", "D", "R", "P", "P", "P"]);
    }

    #[test]
    fn missing_hands_stay_unknown_unless_three_are_complete() {
        // BC: [Deal "S:962.AJ7.KT82.J75 ... ... ..."] — East is not the other 39.
        let one = parse_lin(include_str!("../../fixtures/lin/read-one-hand.lin")).unwrap();
        assert_eq!(hand_lengths(&one), [13, 0, 0, 0]);

        // BC: [Deal "S:962.AJ7.KT82.J75 ... K843.T542.J6.863 ..."].
        let two = parse_lin(include_str!("../../fixtures/lin/read-two-hands.lin")).unwrap();
        assert_eq!(hand_lengths(&two), [13, 0, 13, 0]);
    }

    #[test]
    fn an_alert_with_no_explanation() {
        // BC: `1NT $1 Pass 2C $1 Pass =1= $1` and [Note "1:no stopper"].
        let data = parse_lin(include_str!(
            "../../fixtures/lin/read-alert-without-text.lin"
        ))
        .unwrap();
        let read: Vec<(&str, bool, Option<&str>)> = data
            .auction
            .iter()
            .map(|b| (b.bid.as_str(), b.alert, b.annotation.as_deref()))
            .collect();
        assert_eq!(
            read,
            [
                ("1n", true, None),
                ("p", false, None),
                ("2c", true, None),
                ("p", true, Some("no stopper")),
                ("2d", false, None),
            ]
        );
    }

    #[test]
    fn a_file_without_qx_is_one_board_per_line() {
        let boards = parse_lin_file(include_str!(
            "../../fixtures/lin/read-one-board-per-line.lin"
        ))
        .unwrap();
        let headers: Vec<_> = boards.iter().map(|b| b.board_header.as_deref()).collect();
        assert_eq!(headers, [Some("Board 11"), Some("Board 12")]);
    }

    #[test]
    fn a_bridge_composer_board_spans_several_lines() {
        let boards =
            parse_lin_file(include_str!("../../fixtures/lin/alerts-notes-play-bc.lin")).unwrap();
        assert_eq!(boards.len(), 1);
        let board = &boards[0];
        assert_eq!(board.title.as_deref(), Some("LIN probe - 2026.09.14"));
        assert_eq!(board.room_board.as_deref(), Some("o1,BOARD 1"));
        assert_eq!(board.auction.len(), 11);
        assert_eq!(board.play.len(), 12);
        assert_eq!(board.claim, Some(9));
    }

    #[test]
    fn a_tournament_header_is_not_a_board() {
        let boards = parse_lin_file(include_str!("../../fixtures/lin/two-boards-bc.lin")).unwrap();
        let headers: Vec<_> = boards.iter().map(|b| b.board_header.as_deref()).collect();
        assert_eq!(headers, [Some("Board 5"), Some("Board 6")]);
        assert_eq!(boards[0].title.as_deref(), Some("Void probe"));
        assert_eq!(boards[0].claim, Some(13));
        assert_eq!(boards[1].title, None);
        assert_eq!(boards[1].claim, None);
    }

    #[test]
    fn a_movie_reads_as_its_board() {
        let data = parse_lin(include_str!("../../fixtures/lin/movie-constructs.lin")).unwrap();
        // The overlay `md|0,…|` does not replace the deal.
        assert_eq!(hand_lengths(&data), [13; 4]);
        assert_eq!(data.auction.len(), 6);
        // Only the plays that name one card: not `pc|h|`, `pc|!da|` or `pc|s2hdc|`.
        let play: Vec<String> = data
            .play
            .iter()
            .map(|c| format!("{}{}", c.suit.to_char(), c.rank.to_char()))
            .collect();
        assert_eq!(play, ["HQ", "HA", "CK"]);
    }
}
