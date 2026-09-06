//! ZRD and ZDD: Richard Pavlicek's binary deal and double-dummy library.
//!
//! A `.zrd` record is 23 bytes holding a deal and its complete twenty-cell
//! double-dummy table; a `.zdd` record is the 10-byte table alone. Pavlicek's
//! published library runs to 10,485,760 records, so [`ZrdReader`] and
//! [`ZddReader`] address a record by ordinal and seek to it rather than walking
//! the file.
//!
//! # This module packs and unpacks; it does not adjudicate
//!
//! Nothing here checks that a table is the *right* table for its deal. Deciding
//! whether twenty numbers are the correct double-dummy result is a solver's
//! work, not an encoding's. What is checked is that the bytes formed a
//! well-formed record: fifty-two cards split thirteen to a seat, and no trick
//! count above thirteen.
//!
//! # The record
//!
//! | bytes | meaning |
//! |---|---|
//! | 0-12 | two bits per card, the seat holding it: `00` West, `01` North, `10` East, `11` South. Card order is SA, SK, … S2, then the same descending run for hearts, diamonds, clubs |
//! | 13-22 | four bits per cell, two bytes per strain, strains in the order NT, S, H, D, C, and within each strain the seats in the order W, N, E, S |
//!
//! Both orders differ from this crate's own. [`bridge_types::STRAINS`] runs
//! clubs to notrump — the reverse of this, with notrump moved — and
//! [`bridge_types::DECLARERS`] runs N, E, S, W. Neither mismatch fails loudly:
//! a transposed axis yields a plausible table, and for the seat axis it yields
//! the *opponents'* plausible table. So the orders are named here, at the point
//! they are read and written, and never assumed.
//!
//! # Bit numbering: least significant first
//!
//! Bits are numbered from zero upward, least significant first, which is
//! Pavlicek's stated convention across his binary formats. The ace of spades is
//! therefore the **low** two bits of card byte 0, and the first seat of a strain
//! is the **low** nibble of that strain's first byte.
//!
//! This is the detail most easily got backwards, and it fails silently: reading
//! the two-bit fields most-significant-first still yields four thirteen-card
//! hands, because reversing the fields within a byte only permutes which seat
//! receives which card. The fixture test in this module is the guard.
//!
//! # Two sentinels, both structurally impossible rather than conventional
//!
//! - **A separator record** carries zero in its first four card bytes, which
//!   reads as sixteen cards dealt to West. A hand holds thirteen, so no legal
//!   deal can produce it. Surfaced as [`Record::Separator`] rather than
//!   skipped, because silently dropping a record would shift every later
//!   ordinal — and ordinals are how this file is addressed.
//! - **An all-zero table** means "not solved". Zero is a legitimate cell value,
//!   but an all-zero table is not something a solver can produce; see
//!   [`DdTable::NULL`] for why. A reader returns it as `None`, never as twenty
//!   zeros.

use crate::error::{ParseError, Result};
use bridge_types::{Card, DdTable, Deal, Direction, Hand, Rank, Strain, Suit};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes in one `.zrd` record: 13 of deal, 10 of table.
pub const RECORD_LEN: usize = 23;

/// Bytes in one `.zdd` record, and in the table half of a `.zrd` record.
pub const TABLE_LEN: usize = 10;

/// Bytes of deal at the front of a `.zrd` record.
const DEAL_LEN: usize = RECORD_LEN - TABLE_LEN;

/// Seat order for both the two-bit card codes and the four-bit trick cells:
/// West, North, East, South.
///
/// Not [`bridge_types::DECLARERS`], which is N, E, S, W. Transposing the two
/// swaps each partnership's cells for its opponents', which is invisible in the
/// numbers — every value stays in range and the table still looks like a table.
const SEAT_ORDER: [Direction; 4] = [
    Direction::West,
    Direction::North,
    Direction::East,
    Direction::South,
];

/// Strain order for the trick cells: notrump first, then spades down to clubs.
///
/// Not [`bridge_types::STRAINS`], which runs clubs up to notrump.
const STRAIN_ORDER: [Strain; 5] = [
    Strain::NoTrump,
    Strain::Spades,
    Strain::Hearts,
    Strain::Diamonds,
    Strain::Clubs,
];

/// Suit order of the card bits: spades, hearts, diamonds, clubs.
const CARD_SUITS: [Suit; 4] = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];

/// Rank order within each suit's card bits: ace down to two.
const CARD_RANKS: [Rank; 13] = [
    Rank::Ace,
    Rank::King,
    Rank::Queen,
    Rank::Jack,
    Rank::Ten,
    Rank::Nine,
    Rank::Eight,
    Rank::Seven,
    Rank::Six,
    Rank::Five,
    Rank::Four,
    Rank::Three,
    Rank::Two,
];

/// One record of a `.zrd` file.
///
/// No `PartialEq`: [`bridge_types::Deal`] does not implement it, so comparing
/// two records means comparing what you care about — the hands, the table, or
/// both.
#[derive(Debug, Clone)]
pub enum Record {
    /// A deal, with its table when the record carries one.
    ///
    /// `table` is `None` for the all-zero table, which the format uses to mean
    /// "not solved". It is never `Some` of a null table.
    Deal {
        /// The four hands.
        deal: Deal,
        /// The twenty results, or `None` when the record is unsolved.
        table: Option<DdTable>,
    },
    /// A separator: sixteen cards to one seat, which no legal deal can produce.
    ///
    /// Pavlicek uses these to divide a file into sections. They occupy an
    /// ordinal like any other record.
    Separator,
}

/// The card at `index` in the record's card order, ace of spades first.
fn card_at(index: usize) -> Card {
    Card::new(CARD_SUITS[index / 13], CARD_RANKS[index % 13])
}

/// A card's position in the record's card order, the inverse of [`card_at`].
fn card_index(card: Card) -> usize {
    // Suit is Clubs = 0 … Spades = 3, and the record runs spades first.
    let suit = 3 - (card.suit as usize);
    // Rank is Two = 2 … Ace = 14, and the record runs ace first.
    let rank = 14 - (card.rank as usize);
    suit * 13 + rank
}

/// A seat's two-bit code, and its nibble position within a strain.
fn seat_code(direction: Direction) -> u8 {
    match direction {
        Direction::West => 0,
        Direction::North => 1,
        Direction::East => 2,
        Direction::South => 3,
    }
}

/// Decode one 23-byte `.zrd` record.
///
/// Rejects a record whose cards do not split thirteen to a seat, or whose cells
/// exceed thirteen tricks. A record with zero in its first four card bytes is
/// [`Record::Separator`].
///
/// ```
/// use bridge_encodings::zrd::{read_record, Record};
///
/// let bytes = [
///     0x72, 0x52, 0x7a, 0x0a, 0xe1, 0x7a, 0xe4, 0xf9, 0x52, 0xe0, 0x41, 0xfc,
///     0x7c, 0x49, 0x49, 0x49, 0x49, 0x2b, 0x2a, 0x39, 0x39, 0x67, 0x67,
/// ];
/// let Record::Deal { deal, table } = read_record(&bytes).unwrap() else {
///     panic!("a deal, not a separator");
/// };
/// assert_eq!(
///     deal.to_pbn(bridge_types::Direction::North),
///     "N:J873.J42.Q65.KT2 AT652.A976.AJ82. Q4.85.KT9.A87643 K9.KQT3.743.QJ95"
/// );
/// assert_eq!(
///     table.unwrap().tricks(bridge_types::Direction::West, bridge_types::Strain::NoTrump),
///     9
/// );
/// ```
pub fn read_record(bytes: &[u8]) -> Result<Record> {
    if bytes.len() != RECORD_LEN {
        return Err(ParseError::Zrd(format!(
            "a .zrd record is {RECORD_LEN} bytes, got {}",
            bytes.len()
        )));
    }
    // Sixteen cards to West: impossible for a hand of thirteen, so this is a
    // separator and not a deal to be decoded.
    if bytes[..4].iter().all(|byte| *byte == 0) {
        return Ok(Record::Separator);
    }
    let deal = read_deal(&bytes[..DEAL_LEN])?;
    let table = read_table(&bytes[DEAL_LEN..])?;
    Ok(Record::Deal { deal, table })
}

/// Encode one 23-byte `.zrd` record.
///
/// Pass `None` for `table` to write the all-zero table, which marks the deal
/// unsolved. That is the intended way to write a deal whose double-dummy
/// results are not known — the choice is explicit here because a caller that
/// hands the file on has no other way to say it.
///
/// ```
/// use bridge_encodings::zrd::{read_record, write_record, Record};
/// use bridge_types::Deal;
///
/// let deal = Deal::from_pbn(
///     "N:J873.J42.Q65.KT2 AT652.A976.AJ82. Q4.85.KT9.A87643 K9.KQT3.743.QJ95",
/// )
/// .unwrap();
/// let bytes = write_record(&deal, None).unwrap();
/// assert_eq!(&bytes[13..], &[0; 10], "an unsolved record has an all-zero table");
///
/// let Record::Deal { table, .. } = read_record(&bytes).unwrap() else {
///     panic!("a deal, not a separator");
/// };
/// assert!(table.is_none(), "an all-zero table reads back as unsolved");
/// ```
pub fn write_record(deal: &Deal, table: Option<&DdTable>) -> Result<[u8; RECORD_LEN]> {
    let mut bytes = [0u8; RECORD_LEN];
    write_deal(deal, &mut bytes[..DEAL_LEN])?;
    write_table_bytes(table, &mut bytes[DEAL_LEN..])?;
    Ok(bytes)
}

/// Decode one 10-byte `.zdd` record: a table with no deal attached.
///
/// `None` is the all-zero table. A `.zdd` file carries results only, so a
/// caller pairs a record with a deal by ordinal or not at all.
pub fn read_zdd_table(bytes: &[u8]) -> Result<Option<DdTable>> {
    if bytes.len() != TABLE_LEN {
        return Err(ParseError::Zrd(format!(
            "a .zdd record is {TABLE_LEN} bytes, got {}",
            bytes.len()
        )));
    }
    read_table(bytes)
}

/// Encode one 10-byte `.zdd` record. `None` writes the all-zero table.
pub fn write_zdd_table(table: Option<&DdTable>) -> Result<[u8; TABLE_LEN]> {
    let mut bytes = [0u8; TABLE_LEN];
    write_table_bytes(table, &mut bytes)?;
    Ok(bytes)
}

/// Decode the 13 card bytes into four hands.
fn read_deal(bytes: &[u8]) -> Result<Deal> {
    let mut cards: [Vec<Card>; 4] = Default::default();
    for (byte_index, byte) in bytes.iter().enumerate() {
        for slot in 0..4 {
            // Least significant first: the earlier card is the lower bit pair.
            let seat = ((byte >> (2 * slot)) & 0b11) as usize;
            cards[seat].push(card_at(byte_index * 4 + slot));
        }
    }

    let mut deal = Deal::new();
    for (seat, held) in SEAT_ORDER.into_iter().zip(cards) {
        if held.len() != 13 {
            return Err(ParseError::Zrd(format!(
                "{seat:?} holds {} cards, not 13",
                held.len()
            )));
        }
        deal.set_hand(seat, Hand::from_cards(held));
    }
    Ok(deal)
}

/// Encode four hands into 13 card bytes.
fn write_deal(deal: &Deal, bytes: &mut [u8]) -> Result<()> {
    let mut owner: [Option<Direction>; 52] = [None; 52];
    for seat in Direction::ALL {
        let hand = deal.hand(seat);
        if hand.len() != 13 {
            return Err(ParseError::Zrd(format!(
                "{seat:?} holds {} cards, not 13",
                hand.len()
            )));
        }
        for card in hand.cards() {
            let index = card_index(*card);
            if let Some(held_by) = owner[index] {
                return Err(ParseError::Zrd(format!(
                    "{card:?} is held by both {held_by:?} and {seat:?}"
                )));
            }
            owner[index] = Some(seat);
        }
    }

    for (index, held_by) in owner.into_iter().enumerate() {
        // Four full hands of thirteen distinct cards account for all 52, so a
        // gap here cannot happen; the match keeps it from becoming an unwrap.
        let Some(seat) = held_by else {
            return Err(ParseError::Zrd(format!(
                "no seat holds {:?}",
                card_at(index)
            )));
        };
        bytes[index / 4] |= seat_code(seat) << (2 * (index % 4));
    }
    Ok(())
}

/// Decode the 10 table bytes, mapping the all-zero table to `None`.
fn read_table(bytes: &[u8]) -> Result<Option<DdTable>> {
    let mut table = DdTable::NULL;
    for (strain_index, strain) in STRAIN_ORDER.into_iter().enumerate() {
        let pair = &bytes[strain_index * 2..strain_index * 2 + 2];
        for (seat_index, seat) in SEAT_ORDER.into_iter().enumerate() {
            // Two seats to a byte, least significant nibble first.
            let byte = pair[seat_index / 2];
            let tricks = if seat_index % 2 == 0 {
                byte & 0x0f
            } else {
                byte >> 4
            };
            if tricks > 13 {
                return Err(ParseError::Zrd(format!(
                    "{seat:?} in {strain:?} has {tricks} tricks, above 13"
                )));
            }
            table.set(seat, strain, tricks);
        }
    }
    Ok((!table.is_null()).then_some(table))
}

/// Encode a table into 10 bytes. `None` writes the all-zero table.
fn write_table_bytes(table: Option<&DdTable>, bytes: &mut [u8]) -> Result<()> {
    let Some(table) = table else {
        bytes.fill(0);
        return Ok(());
    };
    for (strain_index, strain) in STRAIN_ORDER.into_iter().enumerate() {
        for (seat_index, seat) in SEAT_ORDER.into_iter().enumerate() {
            let tricks = table.tricks(seat, strain);
            if tricks > 13 {
                return Err(ParseError::Zrd(format!(
                    "{seat:?} in {strain:?} has {tricks} tricks, which will not fit in four bits"
                )));
            }
            let byte = &mut bytes[strain_index * 2 + seat_index / 2];
            if seat_index % 2 == 0 {
                *byte |= tricks;
            } else {
                *byte |= tricks << 4;
            }
        }
    }
    Ok(())
}

/// A file of fixed-length binary records, addressed by ordinal.
struct RecordFile<R> {
    inner: R,
    stride: u64,
    records: u64,
}

impl<R: Read + Seek> RecordFile<R> {
    fn new(mut inner: R, stride: u64, extension: &str) -> Result<Self> {
        let bytes = inner.seek(SeekFrom::End(0))?;
        if bytes % stride != 0 {
            return Err(ParseError::Zrd(format!(
                "a .{extension} file is a whole number of {stride}-byte records, \
                 but this one is {bytes} bytes with {} left over",
                bytes % stride
            )));
        }
        Ok(Self {
            inner,
            stride,
            records: bytes / stride,
        })
    }

    fn read_into(&mut self, index: u64, buf: &mut [u8]) -> Result<()> {
        if index >= self.records {
            return Err(ParseError::Zrd(format!(
                "record {index} is past the end of a {}-record file",
                self.records
            )));
        }
        self.inner.seek(SeekFrom::Start(index * self.stride))?;
        self.inner.read_exact(buf)?;
        Ok(())
    }
}

/// Reads a `.zrd` file, addressing records by ordinal.
///
/// The published library is 241 MB, so nothing is held in memory but the record
/// in hand. Seeking to a record is what makes a `.zrd` usable as a lookup table
/// rather than a stream.
pub struct ZrdReader<R> {
    file: RecordFile<R>,
}

impl ZrdReader<BufReader<File>> {
    /// Open a `.zrd` file by path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> ZrdReader<R> {
    /// Wrap an open `.zrd` source, measuring how many records it holds.
    pub fn new(inner: R) -> Result<Self> {
        Ok(Self {
            file: RecordFile::new(inner, RECORD_LEN as u64, "zrd")?,
        })
    }

    /// How many records the file holds, separators included.
    pub fn len(&self) -> u64 {
        self.file.records
    }

    /// Whether the file holds no records at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read the record at `index`, counting from zero.
    pub fn record(&mut self, index: u64) -> Result<Record> {
        let mut bytes = [0u8; RECORD_LEN];
        self.file.read_into(index, &mut bytes)?;
        read_record(&bytes)
    }

    /// Every record in order.
    pub fn records(&mut self) -> Records<'_, R> {
        Records {
            reader: self,
            next: 0,
        }
    }
}

/// Every record of a [`ZrdReader`], in file order.
pub struct Records<'a, R> {
    reader: &'a mut ZrdReader<R>,
    next: u64,
}

impl<R: Read + Seek> Iterator for Records<'_, R> {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.reader.len() {
            return None;
        }
        let record = self.reader.record(self.next);
        self.next += 1;
        Some(record)
    }
}

/// Reads a `.zdd` file: double-dummy tables with no deals attached.
///
/// A `.zdd` carries results only. Pairing a table with a deal is the caller's
/// problem, and the only handle the format offers is the ordinal.
pub struct ZddReader<R> {
    file: RecordFile<R>,
}

impl ZddReader<BufReader<File>> {
    /// Open a `.zdd` file by path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> ZddReader<R> {
    /// Wrap an open `.zdd` source, measuring how many records it holds.
    pub fn new(inner: R) -> Result<Self> {
        Ok(Self {
            file: RecordFile::new(inner, TABLE_LEN as u64, "zdd")?,
        })
    }

    /// How many tables the file holds.
    pub fn len(&self) -> u64 {
        self.file.records
    }

    /// Whether the file holds no tables at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read the table at `index`, `None` when that record is unsolved.
    pub fn table(&mut self, index: u64) -> Result<Option<DdTable>> {
        let mut bytes = [0u8; TABLE_LEN];
        self.file.read_into(index, &mut bytes)?;
        read_zdd_table(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The first ten records of Pavlicek's library, as shipped with DealerV2_4.
    const FIXTURE: &[u8] = include_bytes!("../../fixtures/zrd/rpdd_10First.zrd");

    /// Record 0 of the fixture, deal and table, as static expected data.
    ///
    /// Established once by re-solving the deal with DDS and comparing all
    /// twenty cells; see `fixtures/zrd/README.md`. Nothing re-solves here.
    /// These numbers are what pin the two axes down — a transposed strain or
    /// seat order round-trips perfectly and would pass every other test in this
    /// module.
    const RECORD_0_PBN: &str =
        "N:J873.J42.Q65.KT2 AT652.A976.AJ82. Q4.85.KT9.A87643 K9.KQT3.743.QJ95";

    /// Record 0's twenty cells as `(seat, strain, tricks)`.
    const RECORD_0_CELLS: [(Direction, Strain, u8); 20] = [
        (Direction::West, Strain::NoTrump, 9),
        (Direction::North, Strain::NoTrump, 4),
        (Direction::East, Strain::NoTrump, 9),
        (Direction::South, Strain::NoTrump, 4),
        (Direction::West, Strain::Spades, 9),
        (Direction::North, Strain::Spades, 4),
        (Direction::East, Strain::Spades, 9),
        (Direction::South, Strain::Spades, 4),
        (Direction::West, Strain::Hearts, 11),
        (Direction::North, Strain::Hearts, 2),
        (Direction::East, Strain::Hearts, 10),
        (Direction::South, Strain::Hearts, 2),
        (Direction::West, Strain::Diamonds, 9),
        (Direction::North, Strain::Diamonds, 3),
        (Direction::East, Strain::Diamonds, 9),
        (Direction::South, Strain::Diamonds, 3),
        (Direction::West, Strain::Clubs, 7),
        (Direction::North, Strain::Clubs, 6),
        (Direction::East, Strain::Clubs, 7),
        (Direction::South, Strain::Clubs, 6),
    ];

    fn fixture_record(index: usize) -> &'static [u8] {
        &FIXTURE[index * RECORD_LEN..(index + 1) * RECORD_LEN]
    }

    /// A record reduced to something comparable, since [`Record`] is not.
    fn identity(record: &Record) -> (String, Option<DdTable>) {
        match record {
            Record::Deal { deal, table } => (deal.to_pbn(Direction::North), *table),
            Record::Separator => ("separator".to_string(), None),
        }
    }

    fn deal_and_table(bytes: &[u8]) -> (Deal, Option<DdTable>) {
        match read_record(bytes).expect("a well-formed record") {
            Record::Deal { deal, table } => (deal, table),
            Record::Separator => panic!("expected a deal, got a separator"),
        }
    }

    #[test]
    fn the_fixture_is_ten_whole_records() {
        assert_eq!(FIXTURE.len(), 10 * RECORD_LEN);
    }

    #[test]
    fn golden_record_decodes_to_the_expected_deal_and_table() {
        let (deal, table) = deal_and_table(fixture_record(0));
        assert_eq!(deal.to_pbn(Direction::North), RECORD_0_PBN);

        let table = table.expect("record 0 is solved");
        for (seat, strain, tricks) in RECORD_0_CELLS {
            assert_eq!(table.tricks(seat, strain), tricks, "{seat:?} in {strain:?}");
        }
    }

    /// Both axes are checked by asserting a cell that a transposition moves.
    ///
    /// Record 0 gives West nine tricks in notrump and North four, so reading
    /// the seats in `DECLARERS` order would put nine on North. It gives West
    /// nine in notrump and seven in clubs, so reading the strains from clubs
    /// upward would put seven in notrump. Neither error is visible in the
    /// numbers themselves — both leave a table that looks entirely ordinary.
    #[test]
    fn the_seat_and_strain_axes_are_pinned_by_the_golden_record() {
        let (_, table) = deal_and_table(fixture_record(0));
        let table = table.expect("record 0 is solved");

        assert_eq!(table.tricks(Direction::West, Strain::NoTrump), 9);
        assert_eq!(table.tricks(Direction::North, Strain::NoTrump), 4);
        assert_eq!(table.tricks(Direction::West, Strain::Clubs), 7);
    }

    /// The ace of spades is the low two bits of card byte 0.
    ///
    /// Reading the two-bit fields most-significant-first still yields four
    /// thirteen-card hands, so only the identity of a specific card catches it.
    #[test]
    fn cards_are_packed_least_significant_first() {
        let (deal, _) = deal_and_table(fixture_record(0));
        let spade_ace = Card::new(Suit::Spades, Rank::Ace);
        assert!(
            deal.hand(Direction::East).has_card(spade_ace),
            "record 0 byte 0 is 0x72, whose low two bits are 0b10 = East"
        );
        assert_eq!(fixture_record(0)[0] & 0b11, seat_code(Direction::East));
    }

    #[test]
    fn every_fixture_record_round_trips_byte_for_byte() {
        for index in 0..10 {
            let bytes = fixture_record(index);
            let (deal, table) = deal_and_table(bytes);
            let written = write_record(&deal, table.as_ref()).expect("a complete deal");
            assert_eq!(written.as_slice(), bytes, "record {index}");
        }
    }

    #[test]
    fn an_all_zero_table_reads_as_unsolved_rather_than_twenty_zeros() {
        let (deal, _) = deal_and_table(fixture_record(0));
        let bytes = write_record(&deal, None).expect("a complete deal");

        assert_eq!(&bytes[DEAL_LEN..], &[0u8; TABLE_LEN]);

        let (read_back, table) = deal_and_table(&bytes);
        assert_eq!(
            read_back.to_pbn(Direction::North),
            deal.to_pbn(Direction::North)
        );
        assert!(table.is_none(), "an all-zero table is not twenty zeros");
    }

    /// A zero cell is ordinary; only the whole table carries the meaning.
    #[test]
    fn a_table_with_some_zero_cells_is_still_solved() {
        let mut table = DdTable::NULL;
        table.set(Direction::North, Strain::Spades, 7);
        let bytes = write_zdd_table(Some(&table)).unwrap();

        let read = read_zdd_table(&bytes).unwrap().expect("not the null table");
        assert_eq!(read.tricks(Direction::North, Strain::Spades), 7);
        assert_eq!(read.tricks(Direction::South, Strain::Clubs), 0);
    }

    /// Sixteen cards to one seat cannot be a deal, so it is not read as one.
    #[test]
    fn a_separator_record_is_surfaced_rather_than_skipped() {
        let mut bytes = [0u8; RECORD_LEN];
        bytes[4] = 0x55;
        assert!(matches!(read_record(&bytes).unwrap(), Record::Separator));
    }

    /// Silently dropping a separator would shift every later ordinal, and the
    /// ordinal is the whole addressing scheme.
    #[test]
    fn a_separator_occupies_an_ordinal_like_any_other_record() {
        let mut file = vec![0u8; RECORD_LEN];
        file[4] = 0x55;
        file.extend_from_slice(fixture_record(0));

        let mut reader = ZrdReader::new(Cursor::new(file)).unwrap();
        assert_eq!(reader.len(), 2);
        assert!(matches!(reader.record(0).unwrap(), Record::Separator));
        let Record::Deal { deal, .. } = reader.record(1).unwrap() else {
            panic!("record 1 is a deal");
        };
        assert_eq!(deal.to_pbn(Direction::North), RECORD_0_PBN);
    }

    #[test]
    fn cards_that_do_not_split_thirteen_to_a_seat_are_rejected() {
        // Every card to North: thirteen bit pairs of 0b01, and no separator
        // since the first four bytes are non-zero.
        let mut bytes = [0u8; RECORD_LEN];
        for byte in bytes.iter_mut().take(DEAL_LEN) {
            *byte = 0b01_01_01_01;
        }
        // West is checked first and comes up empty, North having taken all 52.
        let err = read_record(&bytes).unwrap_err();
        assert!(
            format!("{err}").contains("West holds 0 cards, not 13"),
            "{err}"
        );
    }

    #[test]
    fn a_cell_above_thirteen_is_rejected() {
        let mut bytes = fixture_record(0).to_vec();
        bytes[DEAL_LEN] = 0x0e; // West in notrump, fourteen tricks.
        let err = read_record(&bytes).unwrap_err();
        assert!(format!("{err}").contains("14 tricks"), "{err}");

        let mut table = DdTable::NULL;
        table.set(Direction::West, Strain::NoTrump, 14);
        assert!(write_zdd_table(Some(&table)).is_err());
    }

    #[test]
    fn a_record_of_the_wrong_length_is_rejected() {
        assert!(read_record(&FIXTURE[..RECORD_LEN - 1]).is_err());
        assert!(read_record(&FIXTURE[..RECORD_LEN + 1]).is_err());
        assert!(read_zdd_table(&[0u8; TABLE_LEN - 1]).is_err());
    }

    #[test]
    fn writing_a_deal_that_is_not_four_full_hands_is_rejected() {
        let mut deal = Deal::from_pbn(RECORD_0_PBN).expect("a complete deal");
        deal.hand_mut(Direction::North)
            .add_card(Card::new(Suit::Clubs, Rank::Three));
        let err = write_record(&deal, None).unwrap_err();
        assert!(format!("{err}").contains("holds 14 cards, not 13"), "{err}");
    }

    #[test]
    fn random_access_agrees_with_reading_in_order() {
        let mut reader = ZrdReader::new(Cursor::new(FIXTURE)).unwrap();
        assert_eq!(reader.len(), 10);
        assert!(!reader.is_empty());

        let sequential: Vec<(String, Option<DdTable>)> = reader
            .records()
            .map(|record| record.map(|record| identity(&record)))
            .collect::<Result<Vec<_>>>()
            .expect("ten well-formed records");
        assert_eq!(sequential.len(), 10);

        // Backwards, so a reader that only ever moved forward would fail.
        for index in (0..10).rev() {
            let record = reader.record(index as u64).unwrap();
            assert_eq!(identity(&record), sequential[index], "record {index}");
        }
    }

    #[test]
    fn a_record_past_the_end_is_an_error_not_a_wrapped_read() {
        let mut reader = ZrdReader::new(Cursor::new(FIXTURE)).unwrap();
        let err = reader.record(10).unwrap_err();
        assert!(format!("{err}").contains("past the end"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_whole_records_is_rejected() {
        let Err(err) = ZrdReader::new(Cursor::new(&FIXTURE[..RECORD_LEN + 3])) else {
            panic!("a part-record file is not a .zrd");
        };
        assert!(format!("{err}").contains("left over"), "{err}");
    }

    /// A `.zdd` is the table half alone, on the same ten-byte layout.
    #[test]
    fn zdd_records_read_as_tables_without_deals() {
        let tables: Vec<u8> = (0..10)
            .flat_map(|index| fixture_record(index)[DEAL_LEN..].to_vec())
            .collect();

        let mut reader = ZddReader::new(Cursor::new(tables)).unwrap();
        assert_eq!(reader.len(), 10);

        let table = reader.table(0).unwrap().expect("record 0 is solved");
        for (seat, strain, tricks) in RECORD_0_CELLS {
            assert_eq!(table.tricks(seat, strain), tricks, "{seat:?} in {strain:?}");
        }
    }

    #[test]
    fn zdd_tables_round_trip() {
        for index in 0..10 {
            let bytes = &fixture_record(index)[DEAL_LEN..];
            let table = read_zdd_table(bytes).unwrap();
            assert_eq!(write_zdd_table(table.as_ref()).unwrap(), bytes, "{index}");
        }
    }
}
