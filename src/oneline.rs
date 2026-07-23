//! Oneline format parser for dealer.exe output.
//!
//! The oneline format is a simple text representation of a deal:
//! ```text
//! n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72
//! ```
//!
//! Each hand is a position character followed by cards in S.H.D.C format.

use crate::error::{ParseError, Result};
use bridge_types::{Card, Deal, Direction, Hand, Rank, Suit};

/// Parse a deal in dealer.exe oneline format
///
/// Format: "n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72"
pub fn parse_oneline(input: &str) -> Result<Deal> {
    let parts: Vec<&str> = input.split_whitespace().collect();

    if parts.len() != 8 {
        return Err(ParseError::Oneline(format!(
            "Expected 8 parts (4 positions + 4 hands), got {}",
            parts.len()
        )));
    }

    let mut deal = Deal::new();

    for i in 0..4 {
        let pos_str = parts[i * 2];
        let hand_str = parts[i * 2 + 1];

        let direction = parse_direction_char(pos_str)?;
        let hand = parse_hand(hand_str)?;

        deal.set_hand(direction, hand);
    }

    Ok(deal)
}

/// Format a deal in oneline format
///
/// Output: "n CARDS e CARDS s CARDS w CARDS\n"
pub fn format_oneline(deal: &Deal) -> String {
    let mut result = String::new();

    for &dir in &[
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ] {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push(direction_char(dir));
        result.push(' ');
        result.push_str(&format_hand(deal.hand(dir)));
    }

    result.push('\n');
    result
}

/// Parse a single character direction (n, e, s, w)
fn parse_direction_char(s: &str) -> Result<Direction> {
    match s.to_lowercase().as_str() {
        "n" => Ok(Direction::North),
        "e" => Ok(Direction::East),
        "s" => Ok(Direction::South),
        "w" => Ok(Direction::West),
        _ => Err(ParseError::Oneline(format!(
            "Invalid direction character: {}",
            s
        ))),
    }
}

/// Get lowercase direction character
fn direction_char(dir: Direction) -> char {
    match dir {
        Direction::North => 'n',
        Direction::East => 'e',
        Direction::South => 's',
        Direction::West => 'w',
    }
}

/// Parse a hand in format: Spades.Hearts.Diamonds.Clubs
fn parse_hand(s: &str) -> Result<Hand> {
    let suits_str: Vec<&str> = s.split('.').collect();
    if suits_str.len() != 4 {
        return Err(ParseError::Oneline(format!(
            "Expected 4 suits separated by dots, got {}",
            suits_str.len()
        )));
    }

    let mut hand = Hand::new();
    let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];

    for (suit_idx, &suit_str) in suits_str.iter().enumerate() {
        let suit = suits[suit_idx];

        // Empty string means void suit
        if suit_str.is_empty() {
            continue;
        }

        for c in suit_str.chars() {
            let rank = parse_rank(c)?;
            hand.add_card(Card::new(suit, rank));
        }
    }

    Ok(hand)
}

/// Format a hand in Spades.Hearts.Diamonds.Clubs format
fn format_hand(hand: &Hand) -> String {
    let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
    let mut result = Vec::new();

    for &suit in &suits {
        let mut cards = hand.cards_in_suit(suit);
        if cards.is_empty() {
            result.push(String::new());
        } else {
            // Sort by rank descending (Ace first)
            cards.sort_by_key(|c| std::cmp::Reverse(c.rank));
            let suit_str: String = cards.iter().map(|c| c.rank.to_char()).collect();
            result.push(suit_str);
        }
    }

    result.join(".")
}

/// Parse a rank character
fn parse_rank(c: char) -> Result<Rank> {
    Rank::from_char(c).ok_or_else(|| ParseError::Oneline(format!("Invalid rank character: {}", c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_oneline() {
        let input = "n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72";

        let deal = parse_oneline(input).unwrap();

        let north = deal.hand(Direction::North);
        assert_eq!(north.len(), 13);
        assert_eq!(north.suit_length(Suit::Spades), 5);
        assert_eq!(north.suit_length(Suit::Hearts), 2);
        assert_eq!(north.suit_length(Suit::Diamonds), 4);
        assert_eq!(north.suit_length(Suit::Clubs), 2);
    }

    #[test]
    fn test_format_oneline() {
        let input = "n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72";

        let deal = parse_oneline(input).unwrap();
        let output = format_oneline(&deal);

        // Parse both and compare
        let reparsed = parse_oneline(&output).unwrap();

        // Verify same HCP for each hand
        for dir in Direction::ALL {
            assert_eq!(deal.hand(dir).hcp(), reparsed.hand(dir).hcp());
        }
    }

    #[test]
    fn test_parse_void_suit() {
        // Spades void in south hand
        let input = "n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s .QJ8.Q95432.AQ97 w J74.T953.T6.K863";

        let deal = parse_oneline(input).unwrap();
        let south = deal.hand(Direction::South);

        assert_eq!(south.suit_length(Suit::Spades), 0);
        assert_eq!(south.len(), 13);
    }

    #[test]
    fn test_round_trip() {
        let input = "n A754.7642.KJ2.A9 e QT.AK95.87.K8652 s K93.J83.QT6543.T w J862.QT.A9.QJ743";

        let deal = parse_oneline(input).unwrap();
        let output = format_oneline(&deal);
        let reparsed = parse_oneline(&output).unwrap();

        // Verify HCP matches
        for dir in Direction::ALL {
            assert_eq!(deal.hand(dir).hcp(), reparsed.hand(dir).hcp());
            assert_eq!(deal.hand(dir).len(), reparsed.hand(dir).len());
        }
    }
}
