//! A LIN record as a [`Board`], the way Bridge Composer 5.118.2 opens LIN:
//! what it reads from each `fixtures/lin/read-*.lin` is the `-bc.pbn` beside
//! it, and the tests here check against those.

use bridge_types::{
    Auction, Board, Call, Direction, PlaySequence, PlayerNames, SectionEnd, Strain, Suit,
};

use super::{LinData, LIN_SEATS};

impl LinData {
    /// The board this record describes, as a PBN reader would give it.
    ///
    /// Following Bridge Composer:
    /// - `pn` gives the players, `md` the dealer and deal, `sv` the
    ///   vulnerability, `mn` the event.
    /// - `ah|Board 7|` gives board 7. Nothing is decoded, so `Board+7` gives
    ///   no number; nor does a header in any other form.
    /// - An alert becomes the `$1` annotation, and an `an` a `=n=` reference
    ///   to a `[Note]`, alerted or not.
    /// - The contract and declarer come from the auction, and the play is laid
    ///   out in tricks from declarer's left. Bridge Composer gives a contract
    ///   only when the record has play or `mc`; this also gives one when the
    ///   auction is complete, since the contract is then known.
    /// - `mc|n|` is the result, and closes the play with `*`. Without it a
    ///   play of all thirteen tricks gives the result it took.
    pub fn to_board(&self) -> Board {
        let mut board = Board::new();
        board.event = self.title.clone();
        if let Some(number) = self
            .board_header
            .as_deref()
            .and_then(|h| h.strip_prefix("Board "))
            .and_then(|n| n.trim().parse::<u32>().ok())
        {
            board.number = Some(number);
            board.board_id = Some(number.to_string());
        }

        let mut names = PlayerNames::new();
        for (seat, name) in LIN_SEATS.iter().zip(&self.player_names) {
            if !name.is_empty() {
                names.set(*seat, name.clone());
            }
        }
        if names.has_any() {
            board.player_names = Some(names);
        }

        board.dealer = Some(self.dealer);
        board.vulnerable = self.vulnerability;
        board.deal = self.deal.clone();

        if !self.auction.is_empty() {
            let auction = self.auction_for_pbn();
            let settled = auction.is_complete() || !self.play.is_empty() || self.claim.is_some();
            if let Some(contract) = auction.final_contract().filter(|_| settled) {
                let doubling = if contract.redoubled {
                    "XX"
                } else if contract.doubled {
                    "X"
                } else {
                    ""
                };
                board.contract = Some(format!(
                    "{}{}{doubling}",
                    contract.level,
                    contract.strain.to_pbn()
                ));
                board.declarer = Some(contract.declarer);
                board.play = self.play_for_pbn(contract.declarer, trump_of(contract.strain));
            }
            board.auction = Some(auction);
        }

        board.result = match (self.claim, &board.play, board.declarer) {
            (Some(tricks), _, _) => i8::try_from(tricks).ok(),
            (None, Some(play), Some(declarer)) if play.is_complete() => {
                i8::try_from(play.tricks_won_by_partnership(declarer)).ok()
            }
            _ => None,
        };
        board
    }

    fn auction_for_pbn(&self) -> Auction {
        let mut auction = Auction::new(self.dealer);
        let mut note = 0u8;
        for bid in &self.auction {
            let Some(call) = lin_call(&bid.bid) else {
                continue;
            };
            let mut annotation = String::new();
            if let Some(text) = &bid.annotation {
                note += 1;
                annotation.push_str(&format!("={note}="));
                auction.add_note(note, text.clone());
            }
            if bid.alert {
                annotation.push_str("$1");
            }
            auction.add_annotated_call(call, (!annotation.is_empty()).then_some(annotation));
        }
        auction
    }

    /// The cards in tricks, each led by the last one's winner. `None` when
    /// nothing was played.
    fn play_for_pbn(&self, declarer: Direction, trump: Option<Suit>) -> Option<PlaySequence> {
        if self.play.is_empty() {
            return None;
        }
        let mut play = PlaySequence::new(declarer.next(), trump);
        for card in &self.play {
            if let Some(last) = play.tricks.last() {
                if last.is_complete() {
                    let next = last.winner.unwrap_or(last.leader);
                    play.start_trick(next);
                }
            }
            play.play_card(*card);
        }
        if self.claim.is_some() {
            play.end = SectionEnd::Terminated;
        }
        Some(play)
    }
}

/// A LIN call — `p`, `d`, `r`, `1n`, `3N`, `2c`, any case — as a [`Call`].
fn lin_call(bid: &str) -> Option<Call> {
    match bid.trim().to_ascii_uppercase().as_str() {
        "P" => Some(Call::Pass),
        "D" => Some(Call::Double),
        "R" => Some(Call::Redouble),
        other => Call::from_pbn(other),
    }
}

fn trump_of(strain: Strain) -> Option<Suit> {
    match strain {
        Strain::Spades => Some(Suit::Spades),
        Strain::Hearts => Some(Suit::Hearts),
        Strain::Diamonds => Some(Suit::Diamonds),
        Strain::Clubs => Some(Suit::Clubs),
        Strain::NoTrump => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::lin::{parse_lin, parse_lin_file};
    use crate::pbn::read_pbn;
    use bridge_types::Board;

    /// What Bridge Composer read from `read-<name>.lin`, and what we read.
    fn both(name: &str, lin: &str, bc: &str) -> Vec<(Board, Board)> {
        let ours: Vec<Board> = parse_lin_file(lin)
            .unwrap()
            .iter()
            .map(|d| d.to_board())
            .collect();
        let theirs = read_pbn(bc).unwrap();
        assert_eq!(ours.len(), theirs.len(), "{name}: board count");
        ours.into_iter().zip(theirs).collect()
    }

    /// The fields Bridge Composer's PBN pins down.
    fn assert_same(name: &str, ours: &Board, theirs: &Board) {
        assert_eq!(ours.number, theirs.number, "{name}: board number");
        assert_eq!(ours.dealer, theirs.dealer, "{name}: dealer");
        assert_eq!(ours.vulnerable, theirs.vulnerable, "{name}: vulnerable");
        assert_eq!(
            ours.deal.to_pbn(bridge_types::Direction::North),
            theirs.deal.to_pbn(bridge_types::Direction::North),
            "{name}: deal"
        );
        for seat in bridge_types::Direction::ALL {
            let name_of = |b: &Board| {
                b.player_names
                    .as_ref()
                    .and_then(|n| n.get(seat).map(str::to_string))
                    .filter(|n| !n.is_empty())
            };
            assert_eq!(name_of(ours), name_of(theirs), "{name}: {seat:?}");
        }
        // Where Bridge Composer leaves a complete auction's contract blank,
        // this gives it (see `to_board`)
        let complete = ours.auction.as_ref().is_some_and(|a| a.is_complete());
        if !(complete && theirs.contract.is_none()) {
            assert_eq!(ours.contract, theirs.contract, "{name}: contract");
            assert_eq!(ours.declarer, theirs.declarer, "{name}: declarer");
        }
        assert_eq!(ours.result, theirs.result, "{name}: result");

        let calls = |b: &Board| {
            b.auction.as_ref().map(|a| {
                a.calls
                    .iter()
                    .map(|c| {
                        let ann: String = c
                            .annotation
                            .as_deref()
                            .unwrap_or("")
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect();
                        format!("{}{ann}", c.call.to_pbn())
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(calls(ours), calls(theirs), "{name}: auction");
        let notes = |b: &Board| {
            b.auction.as_ref().map(|a| {
                let mut n: Vec<_> = a.notes.iter().map(|(k, v)| (*k, v.clone())).collect();
                n.sort();
                n
            })
        };
        assert_eq!(notes(ours), notes(theirs), "{name}: notes");

        let cards = |b: &Board| {
            b.play.as_ref().map(|p| {
                (
                    p.opening_leader,
                    p.end,
                    p.tricks
                        .iter()
                        .map(|t| (t.leader, t.cards))
                        .filter(|(_, c)| c.iter().any(Option::is_some))
                        .collect::<Vec<_>>(),
                )
            })
        };
        assert_eq!(cards(ours), cards(theirs), "{name}: play");
    }

    #[test]
    fn reads_lin_as_bridge_composer_does() {
        for (name, lin, bc) in [
            (
                "plus-alerts-claim",
                include_str!("../../fixtures/lin/read-plus-alerts-claim.lin"),
                include_str!("../../fixtures/lin/read-plus-alerts-claim-bc.pbn"),
            ),
            (
                "four-hands",
                include_str!("../../fixtures/lin/read-four-hands.lin"),
                include_str!("../../fixtures/lin/read-four-hands-bc.pbn"),
            ),
            (
                "one-hand",
                include_str!("../../fixtures/lin/read-one-hand.lin"),
                include_str!("../../fixtures/lin/read-one-hand-bc.pbn"),
            ),
            (
                "two-hands",
                include_str!("../../fixtures/lin/read-two-hands.lin"),
                include_str!("../../fixtures/lin/read-two-hands-bc.pbn"),
            ),
            (
                "percent-encoding",
                include_str!("../../fixtures/lin/read-percent-encoding.lin"),
                include_str!("../../fixtures/lin/read-percent-encoding-bc.pbn"),
            ),
            (
                "alert-without-text",
                include_str!("../../fixtures/lin/read-alert-without-text.lin"),
                include_str!("../../fixtures/lin/read-alert-without-text-bc.pbn"),
            ),
            (
                "one-board-per-line",
                include_str!("../../fixtures/lin/read-one-board-per-line.lin"),
                include_str!("../../fixtures/lin/read-one-board-per-line-bc.pbn"),
            ),
        ] {
            for (ours, theirs) in both(name, lin, bc) {
                assert_same(name, &ours, &theirs);
            }
        }
    }

    #[test]
    fn a_complete_auction_gives_its_contract() {
        let board = parse_lin(include_str!("../../fixtures/lin/read-four-hands.lin"))
            .unwrap()
            .to_board();
        assert_eq!(board.contract.as_deref(), Some("2NTXX"));
        assert_eq!(board.declarer, Some(bridge_types::Direction::South));
        assert!(board.play.is_none());
        assert_eq!(board.result, None);
    }

    #[test]
    fn a_full_play_without_a_claim_gives_the_tricks_it_took() {
        // South holds every spade, West every heart, North every diamond.
        // West leads the ace of hearts; from then on South leads a spade.
        let ranks = "AKQJT98765432";
        let mut lin = String::from(
            "pn|S,W,N,E|md|1SAKQJT98765432,HAKQJT98765432,DAKQJT98765432,|sv|o|\
             mb|7S|mb|p|mb|p|mb|p|pc|HA|pc|DA|pc|CA|pc|SA|",
        );
        for r in ranks.chars().skip(1) {
            for suit in ['S', 'H', 'D', 'C'] {
                lin.push_str(&format!("pc|{suit}{r}|"));
            }
        }
        let board = parse_lin(&lin).unwrap().to_board();
        assert_eq!(board.contract.as_deref(), Some("7S"));
        assert_eq!(board.declarer, Some(bridge_types::Direction::South));
        let play = board.play.as_ref().unwrap();
        assert!(play.is_complete());
        // South's spades win every trick, so South leads from trick two on
        assert_eq!(play.tricks[1].leader, bridge_types::Direction::South);
        assert_eq!(board.result, Some(13));
    }
}
