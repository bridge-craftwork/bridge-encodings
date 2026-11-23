//! The `DoubleDummyPlay` section: card-by-card double-dummy costs of a play.
//!
//! The specification is `docs/double-dummy-play.md`; this module is its
//! reference reader and writer. An entry names no cards, only a cost per
//! legal card in hand order, so both directions need the deal and the play
//! to know which cards those are.

use crate::error::{ParseError, Result};
use bridge_types::{
    Card, CardCosts, Deal, Direction, DoubleDummyPlay, PlaySequence, SectionEnd, Suit, TrickCosts,
};

/// Suit order of an entry's groups, as in `[Deal]`.
const SUITS: [Suit; 4] = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];

/// Highest cost an entry can hold: thirteen tricks, `d`.
const MAX_COST: u8 = 13;

/// Decode a `DoubleDummyPlay` section's data tokens.
///
/// `leader` is the section tag's value, and must be `play`'s opening leader.
/// Fails on anything the specification rules out; the caller drops the
/// section whole when it does.
pub fn double_dummy_play_from_pbn(
    leader: Direction,
    tokens: &[String],
    deal: &Deal,
    play: &PlaySequence,
) -> Result<DoubleDummyPlay> {
    if leader != play.opening_leader {
        return Err(invalid(format!(
            "opening leader {} does not match the play's {}",
            leader.to_char(),
            play.opening_leader.to_char()
        )));
    }
    let mut tricks: Vec<TrickCosts> = play
        .tricks
        .iter()
        .map(|t| TrickCosts {
            leader: t.leader,
            turns: [None, None, None, None],
        })
        .collect();
    let mut end = SectionEnd::Unmarked;

    for (k, tok) in tokens.iter().enumerate() {
        if let Some(marker) = SectionEnd::from_pbn(tok) {
            end = marker;
            break;
        }
        let trick_index = k / 4;
        let seat = leader.clockwise_from()[k % 4];
        let trick = play
            .tricks
            .get(trick_index)
            .ok_or_else(|| invalid(format!("entry {} has no trick {}", k + 1, trick_index + 1)))?;
        let slot = (seat.to_index() + 4 - trick.leader.to_index()) % 4;
        let played = trick.cards[slot];

        if tok == "-" {
            continue;
        }
        let Some(played) = played else {
            return Err(invalid(format!(
                "entry {} analyses a card not on record",
                k + 1
            )));
        };
        let held = hand_at(deal, play, seat, trick_index);
        let legal = legal_suits(&held, trick.lead_suit, slot)
            .ok_or_else(|| invalid(format!("entry {}: the lead is not on record", k + 1)))?;
        let costs = decode_entry(tok, &held, &legal)
            .map_err(|why| invalid(format!("entry {} `{tok}`: {why}", k + 1)))?;
        if !costs.cards.iter().any(|(c, _)| *c == played) {
            return Err(invalid(format!(
                "entry {}: the card played is not among its legal cards",
                k + 1
            )));
        }
        tricks[trick_index].turns[slot] = Some(costs);
    }

    Ok(DoubleDummyPlay {
        opening_leader: leader,
        tricks,
        end,
    })
}

/// Encode an analysis as the section's data lines, one trick to a line and
/// its entries in `[Play]`'s column order.
///
/// Fails when the analysis does not fit the deal and play: a turn listing a
/// card the player did not hold, or not listing one they could play.
pub fn double_dummy_play_to_pbn(
    analysis: &DoubleDummyPlay,
    deal: &Deal,
    play: &PlaySequence,
) -> Result<Vec<String>> {
    let leader = play.opening_leader;
    let mut lines = Vec::new();
    for (trick_index, trick) in play.tricks.iter().enumerate() {
        // A play that closes with `*` after a full trick reads as one more,
        // empty trick; it has no line in `[Play]`, so none here either.
        if trick.cards.iter().all(Option::is_none) {
            continue;
        }
        let mut entries = Vec::with_capacity(4);
        for column in 0..4 {
            let seat = leader.clockwise_from()[column];
            let slot = (seat.to_index() + 4 - trick.leader.to_index()) % 4;
            let turn = analysis
                .tricks
                .get(trick_index)
                .and_then(|t| t.turns[slot].as_ref());
            let (Some(turn), Some(_)) = (turn, trick.cards[slot]) else {
                entries.push("-".to_string());
                continue;
            };
            let held = hand_at(deal, play, seat, trick_index);
            let legal = legal_suits(&held, trick.lead_suit, slot).ok_or_else(|| {
                invalid(format!(
                    "trick {}: the lead is not on record",
                    trick_index + 1
                ))
            })?;
            entries.push(encode_entry(turn, &held, &legal).map_err(|why| {
                invalid(format!(
                    "trick {}, {}: {why}",
                    trick_index + 1,
                    seat.to_char()
                ))
            })?);
        }
        lines.push(entries.join(" "));
    }
    if let Some(marker) = analysis.end.to_pbn() {
        lines.push(marker.to_string());
    }
    Ok(lines)
}

/// The cards `seat` holds when trick `trick_index` begins: its hand in the
/// deal less what it played to earlier tricks, each suit high to low.
fn hand_at(
    deal: &Deal,
    play: &PlaySequence,
    seat: Direction,
    trick_index: usize,
) -> [Vec<Card>; 4] {
    let gone: Vec<Card> = play.tricks[..trick_index]
        .iter()
        .filter_map(|t| t.card_by(seat))
        .collect();
    SUITS.map(|suit| {
        let mut cards: Vec<Card> = deal
            .hand(seat)
            .cards_in_suit(suit)
            .into_iter()
            .filter(|c| !gone.contains(c))
            .collect();
        cards.sort_by_key(|c| std::cmp::Reverse(c.rank));
        cards
    })
}

/// Which suits may be played at this turn: all of them when leading or
/// void in the suit led, only that suit otherwise. `None` when a follower's
/// lead is not on record, so legality cannot be known.
fn legal_suits(held: &[Vec<Card>; 4], lead: Option<Suit>, slot: usize) -> Option<[bool; 4]> {
    if slot == 0 {
        return Some([true; 4]);
    }
    let lead = lead?;
    let follows = SUITS
        .iter()
        .position(|s| *s == lead)
        .is_some_and(|i| !held[i].is_empty());
    Some(SUITS.map(|s| !follows || s == lead))
}

fn decode_entry(
    tok: &str,
    held: &[Vec<Card>; 4],
    legal: &[bool; 4],
) -> std::result::Result<CardCosts, String> {
    let groups: Vec<&str> = tok.split('.').collect();
    if groups.len() != 4 {
        return Err("needs four suit groups".into());
    }
    let mut cards = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        let expected = if legal[i] { held[i].len() } else { 0 };
        let found = group.chars().count();
        if found != expected {
            return Err(format!(
                "{} group has {found} cards, the hand {expected} legal",
                SUITS[i].to_char()
            ));
        }
        for (card, ch) in held[i].iter().zip(group.chars()) {
            cards.push((*card, decode_cost(ch)?));
        }
    }
    Ok(CardCosts { cards })
}

fn encode_entry(
    turn: &CardCosts,
    held: &[Vec<Card>; 4],
    legal: &[bool; 4],
) -> std::result::Result<String, String> {
    let legal_count: usize = (0..4).filter(|&i| legal[i]).map(|i| held[i].len()).sum();
    if turn.cards.len() != legal_count {
        return Err(format!(
            "lists {} cards for {legal_count} legal",
            turn.cards.len()
        ));
    }
    let mut groups = Vec::with_capacity(4);
    for i in 0..4 {
        let mut group = String::new();
        if legal[i] {
            for card in &held[i] {
                let (_, cost) = turn
                    .cards
                    .iter()
                    .find(|(c, _)| c == card)
                    .ok_or_else(|| format!("leaves out {}", card_name(*card)))?;
                group.push(encode_cost(*cost)?);
            }
        }
        groups.push(group);
    }
    Ok(groups.join("."))
}

fn decode_cost(ch: char) -> std::result::Result<Option<u8>, String> {
    if ch == '?' {
        return Ok(None);
    }
    match ch.to_digit(16) {
        Some(d) if d <= MAX_COST as u32 => Ok(Some(d as u8)),
        _ => Err(format!("`{ch}` is not a cost")),
    }
}

fn encode_cost(cost: Option<u8>) -> std::result::Result<char, String> {
    match cost {
        None => Ok('?'),
        Some(c) if c <= MAX_COST => {
            char::from_digit(u32::from(c), 16).ok_or_else(|| format!("{c} is not a cost"))
        }
        Some(c) => Err(format!("{c} is more than thirteen tricks")),
    }
}

fn card_name(card: Card) -> String {
    format!("{}{}", card.suit.to_char(), card.rank.to_char())
}

fn invalid(message: String) -> ParseError {
    ParseError::Pbn(format!("DoubleDummyPlay: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbn::read_pbn;

    /// Board 1 of a Brill daily IMP export, with the first two tricks.
    const RECORD: &str = r#"[Board "1"]
[Dealer "N"]
[Deal "N:QJ7.8.T9752.J752 K542.T7.QJ6.Q943 A863.Q932.K84.K8 T9.AKJ654.A3.AT6"]
[Declarer "S"]
[Contract "3D"]
[Play "W"]
HA H8 H7 H2
ST SJ S5 S3
*
"#;

    fn board() -> bridge_types::Board {
        read_pbn(RECORD).unwrap().remove(0)
    }

    fn tokens(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn decodes_each_turn_against_the_hand() {
        let b = board();
        let play = b.play.as_ref().unwrap();
        let data = tokens(
            "11.000000.00.111 .0.. .00.. .0001..\n00.00000.00.000 000... 0000... 0010...\n*",
        );
        let dd = double_dummy_play_from_pbn(Direction::West, &data, &b.deal, play).unwrap();

        // West's lead: thirteen cards, the spades and clubs giving one away
        let lead = dd.turn(0, Direction::West).unwrap();
        assert_eq!(lead.cards.len(), 13);
        assert_eq!(lead.cost_of(card("HA")), Some(0));
        assert_eq!(lead.cost_of(card("ST")), Some(1));
        assert_eq!(lead.cost_of(card("C6")), Some(1));

        // South follows to hearts with four: ♥Q932, the 2 giving one away
        let south = dd.turn(0, Direction::South).unwrap();
        assert_eq!(south.cards.len(), 4);
        assert_eq!(south.cost_of(card("H2")), Some(1));
        assert_eq!(south.cost_of(card("H9")), Some(0));

        // Trick two: West has played the ♥A, so holds five hearts
        assert_eq!(dd.turn(1, Direction::West).unwrap().cards.len(), 12);
        assert_eq!(
            dd.turn(1, Direction::South).unwrap().cost_of(card("S6")),
            Some(1)
        );
        assert_eq!(dd.end, SectionEnd::Terminated);
    }

    #[test]
    fn round_trips() {
        let b = board();
        let play = b.play.as_ref().unwrap();
        let text = "11.000000.00.111 .0.. .00.. .0001..\n00.00000.00.?00 - 0000... 0010...\n*";
        let dd = double_dummy_play_from_pbn(Direction::West, &tokens(text), &b.deal, play).unwrap();
        assert!(dd.turn(1, Direction::North).is_none());
        assert_eq!(
            dd.turn(1, Direction::West).unwrap().cost_of(card("CA")),
            None
        );
        let lines = double_dummy_play_to_pbn(&dd, &b.deal, play).unwrap();
        assert_eq!(lines.join("\n"), text);
    }

    #[test]
    fn refuses_what_the_hand_rules_out() {
        let b = board();
        let play = b.play.as_ref().unwrap();
        for bad in [
            // a group the wrong length
            "11.000000.00.11 .0.. .00.. .0001..",
            // a suit that may not be played while following
            "11.000000.00.111 0.0.. .00.. .0001..",
            // three groups
            "11.000000.00 .0.. .00.. .0001..",
            // a cost past thirteen
            "11.000000.00.11e .0.. .00.. .0001..",
            // a fifth trick's worth of entries
            "- - - - - - - - - - - - 0",
        ] {
            let result = double_dummy_play_from_pbn(Direction::West, &tokens(bad), &b.deal, play);
            assert!(result.is_err(), "{bad}");
        }
        let wrong_leader = double_dummy_play_from_pbn(Direction::North, &[], &b.deal, play);
        assert!(wrong_leader.is_err());
    }

    const SECTION: &str = "[DoubleDummyPlay \"W\"]\n11.000000.00.111 .0.. .00.. .0001..\n00.00000.00.000 000... 0000... 0010...\n*\n";

    #[test]
    fn the_reader_decodes_it_and_the_writer_puts_it_back() {
        let boards = read_pbn(&format!("{RECORD}{SECTION}")).unwrap();
        let dd = boards[0].double_dummy_play.as_ref().unwrap();
        assert_eq!(
            dd.turn(0, Direction::West).unwrap().cost_of(card("ST")),
            Some(1)
        );
        assert!(boards[0]
            .extra_tags
            .iter()
            .all(|(n, _)| n != "DoubleDummyPlay"));

        let written = crate::pbn::write_pbn(&boards);
        assert!(written.contains(SECTION.trim_end()), "{written}");
        let again = read_pbn(&written).unwrap();
        assert_eq!(again[0].double_dummy_play.as_ref(), Some(dd));
    }

    #[test]
    fn it_may_stand_before_the_play() {
        let (head, play) = RECORD.split_at(RECORD.find("[Play").unwrap());
        let boards = read_pbn(&format!("{head}{SECTION}{play}")).unwrap();
        assert!(boards[0].double_dummy_play.is_some());
        assert!(boards[0].play.is_some());
    }

    #[test]
    fn a_section_that_does_not_fit_is_dropped_whole() {
        let bad = SECTION.replace(".0001..", ".001..");
        let boards = read_pbn(&format!("{RECORD}{bad}")).unwrap();
        assert!(boards[0].double_dummy_play.is_none());
        assert!(boards[0].play.is_some(), "the play itself still reads");
        let written = crate::pbn::write_pbn(&boards);
        assert!(!written.contains("DoubleDummyPlay"), "{written}");
    }

    fn card(name: &str) -> Card {
        let mut chars = name.chars();
        let suit = Suit::from_char(chars.next().unwrap()).unwrap();
        let rank = bridge_types::Rank::from_char(chars.next().unwrap()).unwrap();
        Card::new(suit, rank)
    }
}
