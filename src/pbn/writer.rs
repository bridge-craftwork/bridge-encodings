//! PBN file writer.

use bridge_types::{Auction, Board, Card, Direction, PlaySequence};

/// Write boards to PBN format
pub fn write_pbn(boards: &[Board]) -> String {
    let mut output = String::new();

    // PBN header
    output.push_str("% PBN 2.1\n");
    output.push_str("% EXPORT\n");
    output.push('\n');

    for (i, board) in boards.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        output.push_str(&board_to_pbn(board));
    }

    output
}

/// Convert a single board to PBN format
pub fn board_to_pbn(board: &Board) -> String {
    let mut lines = Vec::new();

    // Event tag
    if let Some(ref event) = board.event {
        lines.push(format!("[Event \"{}\"]", event));
    } else {
        lines.push("[Event \"\"]".to_string());
    }

    // Site tag
    if let Some(ref site) = board.site {
        lines.push(format!("[Site \"{}\"]", site));
    } else {
        lines.push("[Site \"\"]".to_string());
    }

    // Date tag
    if let Some(ref date) = board.date {
        lines.push(format!("[Date \"{}\"]", date));
    } else {
        lines.push("[Date \"\"]".to_string());
    }

    // Board number
    if let Some(num) = board.number {
        lines.push(format!("[Board \"{}\"]", num));
    }

    // Player names (preserve when present, else empty for hand records)
    for dir in [Direction::West, Direction::North, Direction::East, Direction::South] {
        let name = board
            .player_names
            .as_ref()
            .and_then(|p| p.get(dir))
            .unwrap_or("");
        lines.push(format!("[{} \"{}\"]", direction_tag(dir), name));
    }

    // Dealer
    if let Some(dealer) = board.dealer {
        lines.push(format!("[Dealer \"{}\"]", dealer.to_char()));
    }

    // Vulnerability
    lines.push(format!("[Vulnerable \"{}\"]", board.vulnerable.to_pbn()));

    // Deal
    let first_dir = board.dealer.unwrap_or(Direction::North);
    lines.push(format!("[Deal \"{}\"]", board.deal.to_pbn(first_dir)));

    // Scoring / result block — preserved when present.
    lines.push("[Scoring \"\"]".to_string());
    lines.push(format!(
        "[Declarer \"{}\"]",
        board.declarer.map(|d| d.to_char().to_string()).unwrap_or_default()
    ));
    lines.push(format!("[Contract \"{}\"]", board.contract.as_deref().unwrap_or("")));
    lines.push(format!(
        "[Result \"{}\"]",
        board.result.map(|r| r.to_string()).unwrap_or_default()
    ));

    // Analysis tags if present
    if let Some(ref dd) = board.double_dummy_tricks {
        lines.push(format!("[DoubleDummyTricks \"{}\"]", dd));
    }
    if let Some(ref opt) = board.optimum_score {
        lines.push(format!("[OptimumScore \"{}\"]", opt));
    }
    if let Some(ref par) = board.par_contract {
        lines.push(format!("[ParContract \"{}\"]", par));
    }

    // Supplemental / custom tags, preserved verbatim in encounter order.
    for (name, value) in &board.extra_tags {
        lines.push(format!("[{} \"{}\"]", name, value));
    }

    // Auction section.
    if let Some(ref auction) = board.auction {
        lines.push(format!("[Auction \"{}\"]", auction.dealer.to_char()));
        lines.extend(auction_lines(auction));
    }

    // Play section.
    if let Some(ref play) = board.play {
        lines.push(format!("[Play \"{}\"]", play.opening_leader.to_char()));
        lines.extend(play_lines(play));
    }

    // Commentary blocks.
    for block in &board.commentary {
        lines.push(format!("{{{}}}", block));
    }

    lines.join("\n") + "\n"
}

/// PBN tag name for a seat's player-name tag.
fn direction_tag(dir: Direction) -> &'static str {
    match dir {
        Direction::North => "North",
        Direction::East => "East",
        Direction::South => "South",
        Direction::West => "West",
    }
}

/// Format an auction's calls, four per line (one bidding round per line).
fn auction_lines(auction: &Auction) -> Vec<String> {
    auction
        .calls
        .chunks(4)
        .map(|round| {
            round
                .iter()
                .map(|c| c.call.to_pbn())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Format a play sequence, one trick per line in play order.
fn play_lines(play: &PlaySequence) -> Vec<String> {
    play.tricks
        .iter()
        .map(|trick| {
            trick
                .cards
                .iter()
                .flatten()
                .map(card_to_pbn)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// A card as a PBN token, e.g. `SA`, `HT`, `C2` (ASCII, not suit symbols).
fn card_to_pbn(card: &Card) -> String {
    format!("{}{}", card.suit.to_char(), card.rank.to_char())
}

/// Write boards to a PBN file
pub fn write_pbn_file(boards: &[Board], path: &std::path::Path) -> std::io::Result<()> {
    let content = write_pbn(boards);
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_types::{Deal, Vulnerability};

    #[test]
    fn test_write_simple_board() {
        let deal =
            Deal::from_pbn("N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ")
                .unwrap();
        let board = Board::new()
            .with_number(1)
            .with_dealer(Direction::North)
            .with_vulnerability(Vulnerability::None)
            .with_deal(deal);

        let pbn = board_to_pbn(&board);

        assert!(pbn.contains("[Board \"1\"]"));
        assert!(pbn.contains("[Dealer \"N\"]"));
        assert!(pbn.contains("[Vulnerable \"None\"]"));
        assert!(pbn.contains(
            "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]"
        ));
    }

    #[test]
    fn test_write_pbn_header() {
        let boards = vec![];
        let pbn = write_pbn(&boards);

        assert!(pbn.starts_with("% PBN 2.1\n"));
        assert!(pbn.contains("% EXPORT"));
    }

    #[test]
    fn test_round_trip() {
        use crate::pbn::read_pbn;

        let deal =
            Deal::from_pbn("N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ")
                .unwrap();
        let board = Board::new()
            .with_number(1)
            .with_dealer(Direction::North)
            .with_vulnerability(Vulnerability::None)
            .with_deal(deal);

        let pbn = write_pbn(&[board]);
        let boards = read_pbn(&pbn).unwrap();

        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].number, Some(1));
        assert_eq!(boards[0].dealer, Some(Direction::North));
    }

    #[test]
    fn test_round_trip_rich_content() {
        use crate::pbn::read_pbn;
        use bridge_types::{Auction, Call, Strain};

        let deal =
            Deal::from_pbn("N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ")
                .unwrap();
        let mut auction = Auction::new(Direction::North);
        for call in [
            Call::bid(1, Strain::NoTrump),
            Call::Pass,
            Call::bid(3, Strain::NoTrump),
            Call::Pass,
            Call::Pass,
            Call::Pass,
        ] {
            auction.add_call(call);
        }
        let board = Board::new()
            .with_number(7)
            .with_dealer(Direction::North)
            .with_vulnerability(Vulnerability::None)
            .with_deal(deal)
            .with_declarer(Direction::South)
            .with_contract("3NT".to_string())
            .with_result(9)
            .with_auction(auction)
            .with_commentary("Cash your winners.".to_string())
            .with_extra_tag("SkillPath", "notrump/stayman")
            .with_extra_tag("Difficulty", "2");

        let pbn = write_pbn(&[board]);
        let boards = read_pbn(&pbn).unwrap();
        assert_eq!(boards.len(), 1);
        let b = &boards[0];

        assert_eq!(b.contract.as_deref(), Some("3NT"));
        assert_eq!(b.declarer, Some(Direction::South));
        assert_eq!(b.result, Some(9));
        assert_eq!(b.extra_tag("SkillPath"), Some("notrump/stayman"));
        assert_eq!(b.extra_tag("Difficulty"), Some("2"));
        assert_eq!(b.commentary, vec!["Cash your winners.".to_string()]);
        let a = b.auction.as_ref().expect("auction survives round-trip");
        assert_eq!(a.len(), 6);
        assert_eq!(a.final_contract().unwrap().strain, Strain::NoTrump);
    }
}
