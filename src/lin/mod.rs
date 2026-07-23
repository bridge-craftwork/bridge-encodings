//! LIN (Linear) format parser for BBO hand records.
//!
//! LIN is a pipe-delimited format used by Bridge Base Online to encode
//! complete hand records including deal, auction, and cardplay in URLs.

use crate::error::Result;
use bridge_types::{Card, Deal, Direction, Hand, Rank, Suit, Vulnerability};

/// A bid with optional alert and annotation
#[derive(Debug, Clone)]
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
    /// The deal (all four hands)
    pub deal: Deal,
    /// Vulnerability
    pub vulnerability: Vulnerability,
    /// Board header (e.g., "Board 1")
    pub board_header: Option<String>,
    /// The auction sequence
    pub auction: Vec<BidWithAnnotation>,
    /// All cards played in order
    pub play: Vec<Card>,
    /// Claim (number of tricks), if hand was claimed
    pub claim: Option<u8>,
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

/// Parse a LIN string into LinData
pub fn parse_lin(lin_str: &str) -> Result<LinData> {
    let mut player_names = [String::new(), String::new(), String::new(), String::new()];
    let mut dealer = Direction::North;
    let mut deal = Deal::new();
    let mut vulnerability = Vulnerability::None;
    let mut board_header = None;
    let mut auction = Vec::new();
    let mut play = Vec::new();
    let mut claim = None;

    let tokens: Vec<&str> = lin_str.split('|').collect();
    let mut i = 0;

    while i < tokens.len() {
        let token = tokens[i].trim();

        match token {
            "pn" => {
                if i + 1 < tokens.len() {
                    let names: Vec<&str> = tokens[i + 1].split(',').collect();
                    for (j, name) in names.iter().enumerate().take(4) {
                        player_names[j] = name.to_string();
                    }
                    i += 1;
                }
            }
            "md" => {
                if i + 1 < tokens.len() {
                    let deal_str = tokens[i + 1];
                    if let Some((d, hands)) = parse_md(deal_str) {
                        dealer = d;
                        deal = hands;
                    }
                    i += 1;
                }
            }
            "sv" => {
                if i + 1 < tokens.len() {
                    vulnerability = parse_sv(tokens[i + 1]);
                    i += 1;
                }
            }
            "ah" => {
                if i + 1 < tokens.len() {
                    board_header = Some(tokens[i + 1].replace('+', " "));
                    i += 1;
                }
            }
            "mb" => {
                if i + 1 < tokens.len() {
                    let bid_str = tokens[i + 1];
                    let (bid, alert) = if bid_str.ends_with('!') {
                        (bid_str.trim_end_matches('!').to_string(), true)
                    } else {
                        (bid_str.to_string(), false)
                    };

                    auction.push(BidWithAnnotation {
                        bid,
                        alert,
                        annotation: None,
                    });
                    i += 1;
                }
            }
            "an" => {
                if i + 1 < tokens.len() {
                    let annotation = tokens[i + 1].replace('+', " ");
                    if let Some(last_bid) = auction.last_mut() {
                        last_bid.annotation = Some(annotation);
                    }
                    i += 1;
                }
            }
            "pc" => {
                if i + 1 < tokens.len() {
                    if let Some(card) = parse_card(tokens[i + 1]) {
                        play.push(card);
                    }
                    i += 1;
                }
            }
            "mc" if i + 1 < tokens.len() => {
                claim = tokens[i + 1].parse().ok();
                i += 1;
            }
            _ => {}
        }

        i += 1;
    }

    Ok(LinData {
        player_names,
        dealer,
        deal,
        vulnerability,
        board_header,
        auction,
        play,
        claim,
    })
}

/// Parse the md (make deal) field
/// Format: dealer_digit + hands (3 hands, 4th is implied)
fn parse_md(md_str: &str) -> Option<(Direction, Deal)> {
    if md_str.is_empty() {
        return None;
    }

    // First character is dealer: 1=S, 2=W, 3=N, 4=E (BBO convention)
    let dealer_char = md_str.chars().next()?;
    let dealer = match dealer_char {
        '1' => Direction::South,
        '2' => Direction::West,
        '3' => Direction::North,
        '4' => Direction::East,
        _ => return None,
    };

    let hands_str = &md_str[1..];
    let hand_strs: Vec<&str> = hands_str.split(',').collect();

    if hand_strs.len() < 3 {
        return None;
    }

    let mut deal = Deal::new();
    let directions = [
        Direction::South,
        Direction::West,
        Direction::North,
        Direction::East,
    ];

    for (i, hand_str) in hand_strs.iter().enumerate().take(3) {
        if let Some(hand) = parse_lin_hand(hand_str) {
            deal.set_hand(directions[i], hand);
        }
    }

    // Calculate the 4th hand from the remaining cards
    if let Some(fourth_hand) = calculate_fourth_hand(&deal, directions[3]) {
        deal.set_hand(directions[3], fourth_hand);
    }

    Some((dealer, deal))
}

/// Parse a single hand in LIN format
/// Format: suits concatenated with suit letter prefix (SHDC order)
fn parse_lin_hand(hand_str: &str) -> Option<Hand> {
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

    Some(hand)
}

/// Calculate the fourth hand from the three known hands
fn calculate_fourth_hand(deal: &Deal, fourth_dir: Direction) -> Option<Hand> {
    let mut fourth = Hand::new();

    for suit in Suit::ALL {
        for rank in Rank::ALL {
            let card = Card::new(suit, rank);
            let mut found = false;

            for dir in Direction::ALL {
                if dir != fourth_dir && deal.hand(dir).has_card(card) {
                    found = true;
                    break;
                }
            }

            if !found {
                fourth.add_card(card);
            }
        }
    }

    Some(fourth)
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
fn parse_card(card_str: &str) -> Option<Card> {
    let mut chars = card_str.chars();
    let suit_char = chars.next()?;
    let rank_char = chars.next()?;

    let suit = Suit::from_char(suit_char)?;
    let rank = Rank::from_char(rank_char)?;

    Some(Card::new(suit, rank))
}

/// Parse multiple boards from a LIN file (tournament format)
pub fn parse_lin_file(content: &str) -> Result<Vec<LinData>> {
    let mut boards = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_lin(line) {
            Ok(data) => boards.push(data),
            Err(_) => {
                // Skip malformed lines
            }
        }
    }

    Ok(boards)
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
    fn test_parse_sv() {
        assert_eq!(parse_sv("o"), Vulnerability::None);
        assert_eq!(parse_sv("b"), Vulnerability::Both);
        assert_eq!(parse_sv("n"), Vulnerability::NorthSouth);
        assert_eq!(parse_sv("e"), Vulnerability::EastWest);
    }

    #[test]
    fn test_parse_lin_hand() {
        let hand = parse_lin_hand("SAKQHJT9D8765C432").unwrap();
        assert_eq!(hand.suit_length(Suit::Spades), 3);
        assert_eq!(hand.suit_length(Suit::Hearts), 3);
        assert_eq!(hand.suit_length(Suit::Diamonds), 4);
        assert_eq!(hand.suit_length(Suit::Clubs), 3);
    }

    #[test]
    fn test_parse_lin_basic() {
        let lin = "pn|South,West,North,East|md|3SAKHJD876C5432,S2HQT9DKQ5CKQJT9,SQJT9HA32DAJ2CA8,|sv|o|ah|Board+1|mb|1C|mb|p|pc|D2|pc|DA|pc|D3|pc|D8|";

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
        let lin = "pn|S,W,N,E|md|1SAKHJD876C5432,,,|sv|b|mb|1C!|an|could+be+short|mb|p|mb|1H!|an|5+hearts|";
        let data = parse_lin(lin).unwrap();

        assert_eq!(data.auction.len(), 3);
        assert!(data.auction[0].alert);
        assert_eq!(
            data.auction[0].annotation,
            Some("could be short".to_string())
        );
        assert!(!data.auction[1].alert);
        assert!(data.auction[2].alert);
        assert_eq!(data.auction[2].annotation, Some("5 hearts".to_string()));
    }
}
