//! Printall format: the default dealer.exe output format.
//!
//! This is the "newspaper-style" 4-column format:
//! ```text
//!    1.
//! J 7 3               9 8                 A Q 5 4 2           K T 6
//! 3                   9 6 4 2             K J 8 7             A Q T 5
//! K Q J T 9 8 5       7                   3 2                 A 6 4
//! T 5                 9 8 7 4 3 2         A K                 Q J 6
//! ```
//!
//! Columns are: North, East, South, West (20 chars each).
//! Rows are: Spades, Hearts, Diamonds, Clubs.

use crate::error::{ParseError, Result};
use bridge_types::{Card, Deal, Direction, Hand, Rank, Suit};

/// Column width in the printall format (each position gets 20 chars).
const COLUMN_WIDTH: usize = 20;

/// Format a deal in printall format (newspaper-style 4-column layout).
///
/// The board number line (e.g. "   1.") is included.
pub fn format_printall(deal: &Deal, board_number: usize) -> String {
    let mut result = String::new();

    result.push_str(&format!("{:4}.\n", board_number));

    let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
    let positions = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];

    for &suit in &suits {
        // cards_count tracks card slots used (each slot = 2 chars: "X ").
        // Start at 10 so the first column doesn't get padded.
        let mut cards_count: usize = 10;

        for &dir in &positions {
            // Pad to column boundary (10 card slots = 20 chars)
            while cards_count < 10 {
                result.push_str("  ");
                cards_count += 1;
            }
            cards_count = 0;

            let mut cards = deal.hand(dir).cards_in_suit(suit);
            cards.sort_by_key(|c| std::cmp::Reverse(c.rank));

            if cards.is_empty() {
                result.push_str("- ");
                cards_count = 1;
            } else {
                for card in &cards {
                    result.push(card.rank.to_char());
                    result.push(' ');
                    cards_count += 1;
                }
            }
        }
        result.push('\n');
    }
    result.push('\n');

    result
}

/// Parse a single printall block (one deal) from dealer output.
///
/// Expects the board number line followed by 4 suit lines, then a blank line.
/// Returns the parsed deal and the number of lines consumed.
pub fn parse_printall(lines: &[&str]) -> Result<(Deal, usize)> {
    // Skip blank lines and find the board number line
    let mut idx = 0;
    while idx < lines.len() && lines[idx].trim().is_empty() {
        idx += 1;
    }

    if idx >= lines.len() {
        return Err(ParseError::Pbn("No printall data found".to_string()));
    }

    // Verify board number line (e.g. "   1." or "  42.")
    let header = lines[idx].trim();
    if !header.ends_with('.')
        || header
            .trim_end_matches('.')
            .trim()
            .parse::<usize>()
            .is_err()
    {
        return Err(ParseError::Pbn(format!(
            "Expected board number line (e.g. '   1.'), got: '{}'",
            header
        )));
    }
    idx += 1;

    let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
    let positions = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];

    let mut hands: [Vec<Card>; 4] = [vec![], vec![], vec![], vec![]];

    for &suit in &suits {
        if idx >= lines.len() {
            return Err(ParseError::Pbn(format!(
                "Expected suit line for {:?}, but reached end of input",
                suit
            )));
        }

        let line = lines[idx];
        idx += 1;

        // Parse 4 columns of 20 chars each
        for (col_idx, &dir) in positions.iter().enumerate() {
            let start = col_idx * COLUMN_WIDTH;
            let end = (start + COLUMN_WIDTH).min(line.len());

            let column = if start < line.len() {
                line[start..end].trim()
            } else {
                ""
            };

            // Skip void marker
            if column == "-" || column.is_empty() {
                continue;
            }

            // Parse space-separated rank characters
            let hand_idx = match dir {
                Direction::North => 0,
                Direction::East => 1,
                Direction::South => 2,
                Direction::West => 3,
            };

            for token in column.split_whitespace() {
                for c in token.chars() {
                    let rank = Rank::from_char(c).ok_or_else(|| {
                        ParseError::Pbn(format!("Invalid rank character '{}' in printall", c))
                    })?;
                    hands[hand_idx].push(Card::new(suit, rank));
                }
            }
        }
    }

    // Skip trailing blank line if present
    if idx < lines.len() && lines[idx].trim().is_empty() {
        idx += 1;
    }

    let mut deal = Deal::new();
    for (i, dir) in positions.iter().enumerate() {
        let hand = Hand::from_cards(std::mem::take(&mut hands[i]));
        deal.set_hand(*dir, hand);
    }

    Ok((deal, idx))
}

/// Parse all printall deals from a string (multiple boards).
pub fn parse_printall_string(content: &str) -> Result<Vec<Deal>> {
    let lines: Vec<&str> = content.lines().collect();
    let mut deals = Vec::new();
    let mut pos = 0;

    while pos < lines.len() {
        // Skip blank lines between deals
        if lines[pos].trim().is_empty() {
            pos += 1;
            continue;
        }

        // Skip statistics lines (Generated, Produced, Initial, Time)
        let trimmed = lines[pos].trim();
        if trimmed.starts_with("Generated ")
            || trimmed.starts_with("Produced ")
            || trimmed.starts_with("Initial ")
            || trimmed.starts_with("Time ")
        {
            pos += 1;
            continue;
        }

        match parse_printall(&lines[pos..]) {
            Ok((deal, consumed)) => {
                deals.push(deal);
                pos += consumed;
            }
            Err(_) => {
                // Skip unrecognized lines
                pos += 1;
            }
        }
    }

    Ok(deals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_deal() -> Deal {
        // Create a known deal for testing
        let pbn = "N:J73.3.KQJT985.T5 98.9642.7.987432 AQ542.KJ87.32.AK KT6.AQT5.A64.QJ6";
        Deal::from_pbn(pbn).unwrap()
    }

    #[test]
    fn test_format_printall() {
        let deal = sample_deal();
        let output = format_printall(&deal, 1);

        assert!(output.starts_with("   1.\n"));
        // 6 lines: board number, 4 suits, blank
        assert_eq!(output.lines().count(), 6);
    }

    #[test]
    fn test_round_trip() {
        let deal = sample_deal();
        let output = format_printall(&deal, 1);
        let lines: Vec<&str> = output.lines().collect();
        let (parsed, _) = parse_printall(&lines).unwrap();

        // Verify all hands match
        for dir in Direction::ALL {
            let orig = deal.hand(dir);
            let round = parsed.hand(dir);
            assert_eq!(
                orig.len(),
                round.len(),
                "Hand length mismatch for {:?}",
                dir
            );
            assert_eq!(orig.hcp(), round.hcp(), "HCP mismatch for {:?}", dir);
            for suit in [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs] {
                assert_eq!(
                    orig.suit_length(suit),
                    round.suit_length(suit),
                    "Suit length mismatch for {:?} {:?}",
                    dir,
                    suit
                );
            }
        }
    }

    #[test]
    fn test_parse_printall_string_multiple_boards() {
        let deal1 = sample_deal();
        let deal2 =
            Deal::from_pbn("N:AKQ.AKQ.AKQ.AKQJ T98.T98.T98.T987 765.765.765.654 J432.J432.J432.32")
                .unwrap();

        let output = format!(
            "{}{}",
            format_printall(&deal1, 1),
            format_printall(&deal2, 2)
        );
        let deals = parse_printall_string(&output).unwrap();
        assert_eq!(deals.len(), 2);
    }

    #[test]
    fn test_parse_with_stats_lines() {
        let deal = sample_deal();
        let output = format!(
            "{}Generated 100 hands\nProduced 5 hands\nInitial random seed 42\nTime needed    0.123 sec\n",
            format_printall(&deal, 1)
        );
        let deals = parse_printall_string(&output).unwrap();
        assert_eq!(deals.len(), 1);
    }

    #[test]
    fn test_format_with_void() {
        // Realistic deal with void suits (6-4-3-0 and 5-4-4-0 shapes)
        let deal =
            Deal::from_pbn("N:AKQ976.KJ84.T32. J84.Q97.AK4.QJ87 T53.AT65..AT9654 2.32.QJ98765.K32")
                .unwrap();
        let output = format_printall(&deal, 1);

        // Void should appear as "- "
        assert!(output.contains("- "));

        // Round-trip
        let lines: Vec<&str> = output.lines().collect();
        let (parsed, _) = parse_printall(&lines).unwrap();
        for dir in Direction::ALL {
            assert_eq!(
                deal.hand(dir).len(),
                parsed.hand(dir).len(),
                "Hand length mismatch for {:?}",
                dir
            );
            assert_eq!(
                deal.hand(dir).hcp(),
                parsed.hand(dir).hcp(),
                "HCP mismatch for {:?}",
                dir
            );
        }
    }
}
