//! LIN writer: a board laid out the way Bridge Composer exports it.
//!
//! ```text
//! mn|<title>|pn|S,W,N,E|qx|o1,BOARD 1|rh||ah|Board 1|md|3<S>,<W>,<N>|sv|n|
//! sa|0|mb|1N|mb|p|an|explanation|...|pg||
//! pc|D2|pc|D3|pc|DJ|pc|DQ|pg||          one line per trick
//! mc|9|pg||
//! ```
//!
//! Where Bridge Composer's output would be wrong, this writer departs from it,
//! and says so where it does. A LIN handviewer URL wants the record on one
//! line: the line breaks sit between tokens, so removing them is enough.

use std::collections::HashMap;

use bridge_types::{
    dealer_from_board_number, Board, Call, Card, Deal, Direction, Hand, Suit, Vulnerability,
};

use super::{calculate_fourth_hand, same_cards, BidWithAnnotation, LinData, LIN_SEATS};

/// Write one board.
///
/// Text is written as it is, spaces included. The only changes are the ones a
/// LIN record needs to stay parseable: a `|` becomes `/`, a line break a space,
/// and a comma in a player name is dropped. Bridge Composer writes all three
/// raw, which ends the value early and breaks the rest of the record.
pub fn write_lin(data: &LinData) -> String {
    let names: Vec<String> = data.player_names.iter().map(|n| clean_name(n)).collect();

    let mut out = format!(
        "mn|{}|pn|{}|qx|{}|rh||",
        clean(data.title.as_deref().unwrap_or("")),
        names.join(","),
        clean(data.room_board.as_deref().unwrap_or("")),
    );
    if let Some(header) = &data.board_header {
        out.push_str(&format!("ah|{}|", clean(header)));
    }
    out.push_str(&format!(
        "md|{}|sv|{}|\n",
        encode_md(data.dealer, &data.deal),
        encode_sv(data.vulnerability)
    ));

    out.push_str("sa|0|");
    for bid in &data.auction {
        out.push_str(&format!(
            "mb|{}{}|",
            clean(&bid.bid),
            if bid.alert { "!" } else { "" }
        ));
        if let Some(annotation) = &bid.annotation {
            out.push_str(&format!("an|{}|", clean(annotation)));
        }
    }
    out.push_str("pg||\n");

    for trick in data.play.chunks(4) {
        for card in trick {
            out.push_str(&format!("pc|{}|", encode_card(*card)));
        }
        out.push_str("pg||\n");
    }

    if let Some(tricks) = data.claim {
        out.push_str(&format!("mc|{tricks}|pg||\n"));
    }
    out
}

/// Write boards one after another: the inverse of
/// [`parse_lin_file`](super::parse_lin_file).
///
/// No tournament header (`vg`, `rs`, `pw`, `mp`, `bn`) is written.
pub fn write_lin_file(boards: &[LinData]) -> String {
    boards.iter().map(write_lin).collect()
}

impl LinData {
    /// The LIN record for a board read from PBN.
    ///
    /// - The title is `Event - Date`, `Event` alone without a date, and
    ///   ` - Date` without an event, as Bridge Composer writes it.
    /// - The board is named from its `[Board]` as written, so a lesson id such
    ///   as `1-1` survives: `qx|o1-1,BOARD 1-1|` and `ah|Board 1-1|`.
    /// - A call's annotation becomes its `an` text. A `=n=` note reference
    ///   gives the note's text; a NAG gives the mark it stands for (`$1` is
    ///   `!`, `$2` is `?`); `!` and `?` are kept. Several on one call are
    ///   joined with a space, and a reference to a note the board does not
    ///   have is kept as written. No call is marked alerted.
    /// - The `+` placeholder and blanks of an exercise auction are dropped.
    /// - The play is written in the order it was played; cards not on record
    ///   are skipped.
    /// - `mc` is the `[Result]`. A board without one gets no claim, where
    ///   Bridge Composer writes `mc|0|`, claiming no tricks.
    ///
    /// Bridge Composer refuses to export a board with a non-numeric id, hidden
    /// hands or note references it cannot resolve; this writes all of them.
    pub fn from_board(board: &Board) -> LinData {
        let names = board.player_names.as_ref();
        let name = |seat: Direction| {
            names
                .and_then(|n| match seat {
                    Direction::North => n.north.as_deref(),
                    Direction::East => n.east.as_deref(),
                    Direction::South => n.south.as_deref(),
                    Direction::West => n.west.as_deref(),
                })
                .map(clean_name)
                .unwrap_or_default()
        };

        let label = board
            .board_id
            .clone()
            .filter(|id| !id.is_empty())
            .or_else(|| board.number.map(|n| n.to_string()));

        let dealer = board
            .dealer
            .or_else(|| board.number.map(dealer_from_board_number))
            .unwrap_or(Direction::North);

        let auction = board
            .auction
            .as_ref()
            .map(|a| {
                a.calls
                    .iter()
                    .filter_map(|call| {
                        Some(BidWithAnnotation {
                            bid: encode_call(&call.call)?,
                            alert: false,
                            annotation: call
                                .annotation
                                .as_deref()
                                .and_then(|raw| annotation_text(raw, &a.notes))
                                .map(|text| clean(&text)),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let play = board
            .play
            .as_ref()
            .map(|p| {
                p.tricks
                    .iter()
                    .flat_map(|t| t.cards.iter().flatten().copied())
                    .collect()
            })
            .unwrap_or_default();

        LinData {
            player_names: LIN_SEATS.map(name),
            dealer,
            deal: board.deal.clone(),
            vulnerability: board.vulnerable,
            title: title(board.event.as_deref(), board.date.as_deref()).map(|t| clean(&t)),
            room_board: label.as_ref().map(|id| clean(&format!("o{id},BOARD {id}"))),
            board_header: label.as_ref().map(|id| clean(&format!("Board {id}"))),
            auction,
            play,
            claim: board.result.and_then(|r| u8::try_from(r).ok()),
        }
    }
}

/// `Event - Date`, `Event`, or ` - Date`.
fn title(event: Option<&str>, date: Option<&str>) -> Option<String> {
    let event = event.filter(|e| !e.is_empty());
    match (event, date.filter(|d| !d.is_empty())) {
        (event, Some(date)) => Some(format!("{} - {date}", event.unwrap_or(""))),
        (Some(event), None) => Some(event.to_string()),
        (None, None) => None,
    }
}

/// A value as a LIN record can hold it: no `|`, no line break.
fn clean(text: &str) -> String {
    text.replace('|', "/").replace(['\r', '\n'], " ")
}

/// A player name as `pn` can hold it, where a comma separates the names.
fn clean_name(name: &str) -> String {
    clean(name).replace(',', "")
}

fn encode_call(call: &Call) -> Option<String> {
    match call {
        Call::Pass => Some("p".to_string()),
        Call::Double => Some("d".to_string()),
        Call::Redouble => Some("r".to_string()),
        Call::Bid { level, strain } => Some(format!("{level}{}", strain.to_char())),
        Call::Continue | Call::Blank => None,
    }
}

/// What a call's PBN annotation says, as `an` text.
fn annotation_text(raw: &str, notes: &HashMap<u8, String>) -> Option<String> {
    let words: Vec<String> = annotation_parts(raw)
        .into_iter()
        .filter_map(|part| {
            if let Some(n) = part.strip_prefix('=').and_then(|r| r.strip_suffix('=')) {
                let note = n.parse::<u8>().ok().and_then(|n| notes.get(&n));
                return Some(note.cloned().unwrap_or_else(|| part.to_string()));
            }
            if !part.is_empty() && part.chars().all(|c| c == '!' || c == '?') {
                return Some(part.to_string());
            }
            // PBN 3.5.4: the first six NAGs are the traditional marks.
            let mark = match part {
                "$1" => "!",
                "$2" => "?",
                "$3" => "!!",
                "$4" => "??",
                "$5" => "!?",
                "$6" => "?!",
                _ => return None,
            };
            Some(mark.to_string())
        })
        .filter(|w| !w.is_empty())
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Split a run of annotations (`!=1=`, `=2==3=`, `$2`) into the individual
/// ones. A character that starts none of them is a part of its own, so it can
/// be discarded without swallowing what follows.
fn annotation_parts(annotation: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = annotation;
    while let Some(first) = rest.chars().next() {
        let len = match first {
            '=' => rest[1..].find('=').map_or(rest.len(), |at| at + 2),
            '$' => {
                1 + rest[1..]
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len() - 1)
            }
            // `!?` and `??` are one mark each, not two.
            '!' | '?' => rest
                .find(|c: char| c != '!' && c != '?')
                .unwrap_or(rest.len()),
            other => other.len_utf8(),
        };
        let (part, tail) = rest.split_at(len);
        parts.push(part);
        rest = tail;
    }
    parts
}

/// The dealer digit, then South, West and North. East is left out when a
/// reader would compute it anyway — the other three complete, and East empty
/// or exactly the cards they leave — and written otherwise. An empty field is
/// a hand not on record.
fn encode_md(dealer: Direction, deal: &Deal) -> String {
    let digit = match dealer {
        Direction::South => '1',
        Direction::West => '2',
        Direction::North => '3',
        Direction::East => '4',
    };
    let three_complete = LIN_SEATS[..3].iter().all(|s| deal.hand(*s).len() == 13);
    let seats = if three_complete && east_is_implied(deal) {
        &LIN_SEATS[..3]
    } else {
        &LIN_SEATS[..]
    };
    let hands: Vec<String> = seats.iter().map(|s| encode_hand(deal.hand(*s))).collect();
    format!("{digit}{}", hands.join(","))
}

/// Whether East is what a reader computes from the other three: empty, or
/// exactly the cards they leave. A record that wrote some other East — one
/// card, or a hand that repeats a card — keeps it.
fn east_is_implied(deal: &Deal) -> bool {
    let east = deal.hand(Direction::East);
    east.is_empty() || same_cards(east, &calculate_fourth_hand(deal, Direction::East))
}

/// Suits in S, H, D, C order, ranks high to low. Every suit letter is written,
/// so a void is two letters together (`SHDAKQ…C`).
fn encode_hand(hand: &Hand) -> String {
    if hand.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for suit in [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs] {
        out.push(suit.to_char());
        let mut cards = hand.cards_in_suit(suit);
        cards.sort_by_key(|c| std::cmp::Reverse(c.rank));
        out.extend(cards.iter().map(|c| c.rank.to_char()));
    }
    out
}

fn encode_card(card: Card) -> String {
    format!("{}{}", card.suit.to_char(), card.rank.to_char())
}

fn encode_sv(vulnerability: Vulnerability) -> &'static str {
    match vulnerability {
        Vulnerability::None => "0",
        Vulnerability::NorthSouth => "n",
        Vulnerability::EastWest => "e",
        Vulnerability::Both => "b",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lin::{parse_lin, parse_lin_file};
    use crate::pbn::read_pbn;

    #[test]
    fn an_east_that_is_not_the_remainder_is_written() {
        // Three complete hands and a one-card East, as a record in the wild
        // wrote it. Leaving East out would read back as all thirteen.
        let lin = "md|3S962HAJ7DKT82CJ75,ST5HQ9863DA943CKQ,SK843HT542DJ6C863,SA|";
        let data = parse_lin(lin).unwrap();
        assert_eq!(data.deal.hand(Direction::East).len(), 1);
        let out = write_lin(&data);
        assert!(out.contains(",SAHDC|sv|"), "{out}");
        assert_eq!(parse_lin(&out).unwrap(), data);
    }

    // Each `<name>.pbn` beside a `<name>-bc.lin` is what Bridge Composer 5.118.2
    // exported from it; see fixtures/lin/README.md.
    const ALERTS: &str = include_str!("../../fixtures/lin/alerts-notes-play.pbn");
    const ALERTS_BC: &str = include_str!("../../fixtures/lin/alerts-notes-play-bc.lin");
    const DELIMITERS: &str = include_str!("../../fixtures/lin/names-with-delimiters.pbn");
    const DELIMITERS_BC: &str = include_str!("../../fixtures/lin/names-with-delimiters-bc.lin");
    const CONTINUE: &str = include_str!("../../fixtures/lin/continue-marker.pbn");
    const CONTINUE_BC: &str = include_str!("../../fixtures/lin/continue-marker-bc.lin");
    const NAG: &str = include_str!("../../fixtures/lin/nag-and-notrump.pbn");
    const NAG_BC: &str = include_str!("../../fixtures/lin/nag-and-notrump-bc.lin");
    const VOIDS: &str = include_str!("../../fixtures/lin/voids.pbn");
    const VOIDS_BC: &str = include_str!("../../fixtures/lin/voids-bc.lin");
    const NO_AUCTION: &str = include_str!("../../fixtures/lin/no-auction.pbn");
    const NO_AUCTION_BC: &str = include_str!("../../fixtures/lin/no-auction-bc.lin");
    const DATE_ONLY: &str = include_str!("../../fixtures/lin/date-only.pbn");
    const DATE_ONLY_BC: &str = include_str!("../../fixtures/lin/date-only-bc.lin");
    const TWO_BOARDS: &str = include_str!("../../fixtures/lin/two-boards.pbn");
    const TWO_BOARDS_BC: &str = include_str!("../../fixtures/lin/two-boards-bc.lin");
    const CANNOT_EXPORT: &str = include_str!("../../fixtures/lin/bc-cannot-export.pbn");

    fn boards(pbn: &str) -> Vec<LinData> {
        read_pbn(pbn)
            .unwrap()
            .iter()
            .map(LinData::from_board)
            .collect()
    }

    fn lin_for(pbn: &str) -> String {
        write_lin_file(&boards(pbn))
    }

    /// Bridge Composer writes CRLF; this writer writes LF.
    fn lf(bc: &str) -> String {
        bc.replace("\r\n", "\n")
    }

    #[test]
    fn matches_bridge_composer() {
        for (pbn, bc) in [
            (ALERTS, ALERTS_BC),
            (CONTINUE, CONTINUE_BC),
            (VOIDS, VOIDS_BC),
            (NO_AUCTION, NO_AUCTION_BC),
            (DATE_ONLY, DATE_ONLY_BC),
        ] {
            assert_eq!(lin_for(pbn), lf(bc));
        }
    }

    #[test]
    fn a_trick_is_written_from_its_own_leader() {
        // Trick 2 is led by East, who won trick 1; the PBN lists it from South.
        let out = lin_for(ALERTS);
        let tricks: Vec<&str> = out.lines().filter(|l| l.starts_with("pc|")).collect();
        assert_eq!(tricks[1], "pc|CA|pc|C5|pc|CK|pc|C3|pg||");
    }

    #[test]
    fn a_note_reference_is_written_as_the_note() {
        let out = lin_for(ALERTS);
        assert!(out.contains("mb|1H|an|4+ hearts, could be 5+|"));
        assert!(out.contains("mb|1C|an|!|"), "a bare alert");
        assert!(out.contains("mb|3N|an|?|"), "$2");
    }

    #[test]
    fn a_board_without_a_result_claims_nothing() {
        // Bridge Composer writes `mc|0|` here, as though declarer took no tricks.
        assert_eq!(lin_for(NAG), lf(NAG_BC).replace("mc|0|pg||\n", ""));
    }

    #[test]
    fn delimiters_in_text_do_not_break_the_record() {
        // Bridge Composer writes the comma and the pipes raw.
        let expected = lf(DELIMITERS_BC)
            .replace(
                "A+B,Smith, J,O'Brien,Lee|Kim",
                "A+B,Smith J,O'Brien,Lee/Kim",
            )
            .replace("5+ hearts | 11-15, 100%", "5+ hearts / 11-15, 100%")
            .replace("mc|0|pg||\n", "");
        assert_eq!(lin_for(DELIMITERS), expected);

        let read = parse_lin_file(&expected).unwrap();
        assert_eq!(
            read[0].player_names,
            ["A+B", "Smith J", "O'Brien", "Lee/Kim"]
        );
    }

    #[test]
    fn a_file_is_its_boards_in_order() {
        let out = lin_for(TWO_BOARDS);
        assert_eq!(out, lf(VOIDS_BC) + &lf(NO_AUCTION_BC));

        // Bridge Composer's file adds a tournament header, and gives the
        // board with no auction three passes; its first board reads the same.
        let ours = parse_lin_file(&out).unwrap();
        let theirs = parse_lin_file(TWO_BOARDS_BC).unwrap();
        assert_eq!(ours.len(), 2);
        assert_eq!(ours[0], theirs[0]);
    }

    #[test]
    fn boards_bridge_composer_will_not_export() {
        let out = lin_for(CANNOT_EXPORT);
        assert!(
            out.contains("qx|o1-1,BOARD 1-1|rh||ah|Board 1-1|"),
            "a lesson id"
        );
        assert!(
            out.contains("md|1S962HAJ7DKT82CJ75,,SK843HT542DJ6C863,|"),
            "hidden hands stay hidden:\n{out}"
        );
        assert!(out.contains("mb|3C|an|=1=|"), "a reference to no note");
        assert!(
            out.contains("mb|2N|an|20-21|"),
            "a note not numbered from 1"
        );
        assert!(
            out.contains("mb|2N|an|20-21 or a strong hand|"),
            "two references on one call"
        );
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        for pbn in [
            ALERTS,
            DELIMITERS,
            CONTINUE,
            NAG,
            VOIDS,
            NO_AUCTION,
            DATE_ONLY,
            TWO_BOARDS,
            CANNOT_EXPORT,
        ] {
            let boards = boards(pbn);
            for board in &boards {
                assert_eq!(parse_lin(&write_lin(board)).unwrap(), *board);
            }
            assert_eq!(parse_lin_file(&write_lin_file(&boards)).unwrap(), boards);
        }
    }

    #[test]
    fn a_record_read_from_lin_writes_back_the_same() {
        let lin =
            "pn|S,W,N,E|md|1S962HAJ7DKT82CJ75,,,|sv|b|ah|Board 2|mb|1C!|an|could be short|mb|p|";
        let data = parse_lin(lin).unwrap();
        assert_eq!(parse_lin(&write_lin(&data)).unwrap(), data);
        assert!(write_lin(&data).contains("mb|1C!|an|could be short|"));
    }

    #[test]
    fn annotation_runs() {
        let notes = HashMap::from([(1, "Forcing".to_string()), (2, "Stayman".to_string())]);
        assert_eq!(annotation_text("=1=", &notes).as_deref(), Some("Forcing"));
        assert_eq!(
            annotation_text("!=2=", &notes).as_deref(),
            Some("! Stayman")
        );
        assert_eq!(annotation_text("!?", &notes).as_deref(), Some("!?"));
        assert_eq!(annotation_text("$4", &notes).as_deref(), Some("??"));
        assert_eq!(annotation_text("$17", &notes), None);
        assert_eq!(annotation_text("=9=", &notes).as_deref(), Some("=9="));
    }

    #[test]
    fn titles() {
        assert_eq!(
            title(Some("Club"), Some("2026.09.14")).as_deref(),
            Some("Club - 2026.09.14")
        );
        assert_eq!(title(Some("Club"), Some("")).as_deref(), Some("Club"));
        assert_eq!(
            title(None, Some("2026.09.14")).as_deref(),
            Some(" - 2026.09.14")
        );
        assert_eq!(title(Some(""), None), None);
    }
}
