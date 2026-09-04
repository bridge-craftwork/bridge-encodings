//! The two PBN encodings of a double-dummy table.
//!
//! [`DdTable`] holds the twenty results; this module is the only place that
//! says how they are written down. Both encodings order their cells
//! differently from each other and from the type's own storage, which is
//! exactly why the type refuses to be indexed positionally — every order in
//! play is stated here, at the point it is emitted.
//!
//! # `OptimumResultTable` is standard; `DoubleDummyTricks` is not
//!
//! `OptimumResultTable` is PBN 2.1 §5.7: a table section, a header naming the
//! columns followed by one row per cell. `DoubleDummyTricks` appears nowhere in
//! the specification — it is a Bridge Composer extension, and its authority is
//! what Bridge Composer and the tools around it actually read. Only one of
//! these has a document to be checked against, and it is worth knowing which
//! when a question about them comes up.

use crate::error::{ParseError, Result};
use bridge_types::{DdTable, Direction, Strain};

/// Declarer order for both encodings: North, South, East, West.
///
/// Partners adjacent, which is how a double-dummy table is read. Note this is
/// neither [`bridge_types::DECLARERS`] (`N, E, S, W`, seating order) nor the
/// `W, N, E, S` the PBN specification uses to describe the `Declarer` column's
/// value domain — that is a list of permitted values, not a required row order.
const ROW_ORDER: [Direction; 4] = [
    Direction::North,
    Direction::South,
    Direction::East,
    Direction::West,
];

/// Strain order for both encodings: notrump first, then spades down to clubs.
///
/// **NT first.** This matters and has been got wrong before. It matches what
/// bridgewebs BSOL returns, and what `Bridge-Classroom`'s
/// `handAnalysis.js::buildDdRows` actually renders — its `colSuitIdx =
/// [4,3,2,1,0]` against `colStrain = ['C','D','H','S','NT']` only lines up if
/// character position 0 is notrump. Verified byte-for-byte against bridgewebs
/// for five deals. Some older comments and plan documents in this family say
/// `S, H, D, C, NT`; they are wrong, and the working display code is the
/// authority.
const COLUMN_ORDER: [Strain; 5] = [
    Strain::NoTrump,
    Strain::Spades,
    Strain::Hearts,
    Strain::Diamonds,
    Strain::Clubs,
];

/// The header value of an `OptimumResultTable` section, naming its three
/// columns and their field widths as PBN 2.1 §5.7 defines them.
pub const OPTIMUM_RESULT_TABLE_HEADER: &str = "Declarer;Denomination\\2R;Result\\2R";

/// Encode a table as a `DoubleDummyTricks` tag value: twenty characters, one
/// per cell, in [`ROW_ORDER`] then [`COLUMN_ORDER`].
///
/// Each is a single base-14 digit — `0`-`9` then lowercase `a`-`d` for ten to
/// thirteen. Counts above thirteen cannot occur in bridge and are clamped, so
/// the output is always twenty characters a decoder can read back.
///
/// ```
/// use bridge_encodings::pbn::dd_table_to_pbn;
/// use bridge_types::{DdTable, Direction, Strain};
///
/// let mut table = DdTable::new();
/// table.set(Direction::North, Strain::NoTrump, 13);
/// assert_eq!(&dd_table_to_pbn(&table)[..1], "d");
/// ```
pub fn dd_table_to_pbn(table: &DdTable) -> String {
    let mut out = String::with_capacity(20);
    for declarer in ROW_ORDER {
        for strain in COLUMN_ORDER {
            out.push(encode_tricks(table.tricks(declarer, strain)));
        }
    }
    out
}

/// Decode a `DoubleDummyTricks` tag value.
///
/// Rejects anything that is not exactly twenty valid digits, rather than
/// filling the gaps: a short or malformed value means the producer and this
/// reader disagree, and a half-populated table would carry that disagreement
/// silently into whatever displays it.
pub fn dd_table_from_pbn(value: &str) -> Result<DdTable> {
    let value = value.trim();
    if value.chars().count() != 20 {
        return Err(ParseError::Pbn(format!(
            "DoubleDummyTricks must be 20 characters, got {}",
            value.chars().count()
        )));
    }

    let mut table = DdTable::new();
    let mut chars = value.chars();
    for declarer in ROW_ORDER {
        for strain in COLUMN_ORDER {
            let c = chars.next().unwrap_or('\0');
            let tricks = decode_tricks(c).ok_or_else(|| {
                ParseError::Pbn(format!("invalid trick count {c:?} in DoubleDummyTricks"))
            })?;
            table.set(declarer, strain, tricks);
        }
    }
    Ok(table)
}

/// The twenty data rows of an `OptimumResultTable` section, in
/// [`ROW_ORDER`] then [`COLUMN_ORDER`], formatted to the header's field widths.
///
/// Pair with [`OPTIMUM_RESULT_TABLE_HEADER`] and
/// `PbnDocument::set_section`.
pub fn optimum_result_table_rows(table: &DdTable) -> Vec<String> {
    let mut rows = Vec::with_capacity(20);
    for declarer in ROW_ORDER {
        for strain in COLUMN_ORDER {
            rows.push(format!(
                "{} {:>2} {:>2}",
                declarer.to_char(),
                strain_token(strain),
                table.tricks(declarer, strain)
            ));
        }
    }
    rows
}

/// Read an `OptimumResultTable` section's data rows back into a table.
///
/// Rows may arrive in any order — each names its own declarer and denomination
/// — but all twenty must be present. Anything unparseable is an error rather
/// than a skipped row, for the same reason as [`dd_table_from_pbn`].
pub fn optimum_result_table_from_rows<S: AsRef<str>>(rows: &[S]) -> Result<DdTable> {
    let mut table = DdTable::new();
    let mut seen = [[false; 5]; 4];

    for row in rows {
        let row = row.as_ref().trim();
        if row.is_empty() {
            continue;
        }
        let fields: Vec<&str> = row.split_whitespace().collect();
        let [declarer, denomination, result] = fields[..] else {
            return Err(ParseError::Pbn(format!(
                "OptimumResultTable row wants 3 fields, got {}: {row:?}",
                fields.len()
            )));
        };

        let declarer = declarer
            .chars()
            .next()
            .and_then(Direction::from_char)
            .ok_or_else(|| ParseError::Pbn(format!("bad declarer {declarer:?} in {row:?}")))?;
        let strain = strain_from_token(denomination).ok_or_else(|| {
            ParseError::Pbn(format!("bad denomination {denomination:?} in {row:?}"))
        })?;
        let tricks: u8 = result
            .parse()
            .map_err(|_| ParseError::Pbn(format!("bad result {result:?} in {row:?}")))?;
        if tricks > 13 {
            return Err(ParseError::Pbn(format!(
                "result {tricks} exceeds 13 in {row:?}"
            )));
        }

        table.set(declarer, strain, tricks);
        seen[declarer.to_index()][column_index(strain)] = true;
    }

    let missing = seen.iter().flatten().filter(|s| !**s).count();
    if missing > 0 {
        return Err(ParseError::Pbn(format!(
            "OptimumResultTable is missing {missing} of its 20 cells"
        )));
    }
    Ok(table)
}

/// Whether a line looks like an `OptimumResultTable` data row rather than the
/// next tag or a blank.
///
/// For callers walking a file by hand. Anything holding a
/// [`crate::pbn::PbnDocument`] should use its section handling instead.
pub fn is_optimum_result_row(line: &str) -> bool {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [declarer, denomination, result] = fields[..] else {
        return false;
    };
    declarer.len() == 1
        && declarer
            .chars()
            .next()
            .and_then(Direction::from_char)
            .is_some()
        && strain_from_token(denomination).is_some()
        && result.parse::<u8>().is_ok_and(|t| t <= 13)
}

/// One trick count as a base-14 digit: `0`-`9`, then `a`-`d` for 10-13.
fn encode_tricks(tricks: u8) -> char {
    match tricks.min(13) {
        t @ 0..=9 => (b'0' + t) as char,
        t => (b'a' + (t - 10)) as char,
    }
}

/// The inverse of [`encode_tricks`]. Uppercase `A`-`D` is accepted on the way
/// in, since it costs nothing and some producers shout.
fn decode_tricks(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        'a'..='d' => Some(c as u8 - b'a' + 10),
        'A'..='D' => Some(c as u8 - b'A' + 10),
        _ => None,
    }
}

/// A strain's `Denomination` token: `NT`, `S`, `H`, `D`, `C`.
fn strain_token(strain: Strain) -> &'static str {
    match strain {
        Strain::NoTrump => "NT",
        Strain::Spades => "S",
        Strain::Hearts => "H",
        Strain::Diamonds => "D",
        Strain::Clubs => "C",
    }
}

/// The inverse of [`strain_token`], case-insensitively.
fn strain_from_token(token: &str) -> Option<Strain> {
    match token.to_ascii_uppercase().as_str() {
        "NT" | "N" => Some(Strain::NoTrump),
        "S" => Some(Strain::Spades),
        "H" => Some(Strain::Hearts),
        "D" => Some(Strain::Diamonds),
        "C" => Some(Strain::Clubs),
        _ => None,
    }
}

/// A strain's index within [`COLUMN_ORDER`], for the coverage check.
fn column_index(strain: Strain) -> usize {
    COLUMN_ORDER
        .iter()
        .position(|s| *s == strain)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table whose every cell is distinct, so a transposition cannot pass.
    fn distinct_table() -> DdTable {
        let mut n = 0u8;
        DdTable::from_fn(|_, _| {
            n += 1;
            n % 14
        })
    }

    #[test]
    fn dd_tricks_round_trips() {
        let table = distinct_table();
        let encoded = dd_table_to_pbn(&table);
        assert_eq!(encoded.len(), 20);
        assert_eq!(dd_table_from_pbn(&encoded).unwrap(), table);
    }

    /// The first character is North in notrump — the fact the bridgewebs
    /// comparison pinned, and the one a reordering would break first.
    #[test]
    fn first_character_is_north_notrump() {
        let mut table = DdTable::new();
        table.set(Direction::North, Strain::NoTrump, 7);
        assert_eq!(dd_table_to_pbn(&table).chars().next(), Some('7'));

        // ...and the second is North in spades, not South in notrump.
        table.set(Direction::North, Strain::Spades, 5);
        assert_eq!(dd_table_to_pbn(&table).chars().nth(1), Some('5'));
    }

    #[test]
    fn ten_to_thirteen_use_letters() {
        let mut table = DdTable::new();
        for (i, declarer) in ROW_ORDER.iter().enumerate() {
            table.set(*declarer, Strain::NoTrump, 10 + i as u8);
        }
        let encoded = dd_table_to_pbn(&table);
        assert_eq!(encoded.chars().next(), Some('a'));
        assert_eq!(encoded.chars().nth(5), Some('b'));
        assert_eq!(encoded.chars().nth(10), Some('c'));
        assert_eq!(encoded.chars().nth(15), Some('d'));
    }

    #[test]
    fn uppercase_letters_decode() {
        let lower = dd_table_from_pbn("abcd0000000000000000").unwrap();
        let upper = dd_table_from_pbn("ABCD0000000000000000").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn a_malformed_dd_tricks_value_is_an_error_not_a_partial_table() {
        assert!(dd_table_from_pbn("").is_err());
        assert!(dd_table_from_pbn("123").is_err());
        assert!(dd_table_from_pbn("0000000000000000000").is_err()); // 19
        assert!(dd_table_from_pbn("000000000000000000000").is_err()); // 21
        assert!(dd_table_from_pbn("z0000000000000000000").is_err());
        assert!(dd_table_from_pbn("e0000000000000000000").is_err()); // 14 tricks
    }

    #[test]
    fn optimum_result_table_round_trips() {
        let table = distinct_table();
        let rows = optimum_result_table_rows(&table);
        assert_eq!(rows.len(), 20);
        assert_eq!(optimum_result_table_from_rows(&rows).unwrap(), table);
    }

    #[test]
    fn optimum_result_rows_are_the_shape_bridge_composer_writes() {
        let mut table = DdTable::new();
        table.set(Direction::North, Strain::NoTrump, 9);
        table.set(Direction::North, Strain::Spades, 7);
        let rows = optimum_result_table_rows(&table);
        assert_eq!(rows[0], "N NT  9");
        assert_eq!(rows[1], "N  S  7");
    }

    /// Rows carry their own coordinates, so order is not load-bearing.
    #[test]
    fn optimum_result_rows_may_arrive_in_any_order() {
        let table = distinct_table();
        let mut rows = optimum_result_table_rows(&table);
        rows.reverse();
        assert_eq!(optimum_result_table_from_rows(&rows).unwrap(), table);
    }

    #[test]
    fn an_incomplete_optimum_result_table_is_an_error() {
        let table = distinct_table();
        let mut rows = optimum_result_table_rows(&table);
        rows.pop();
        let err = optimum_result_table_from_rows(&rows).unwrap_err();
        assert!(format!("{err}").contains("missing 1"), "{err}");
    }

    #[test]
    fn bad_optimum_result_rows_are_rejected() {
        assert!(optimum_result_table_from_rows(&["N NT"]).is_err());
        assert!(optimum_result_table_from_rows(&["X NT 9"]).is_err());
        assert!(optimum_result_table_from_rows(&["N XX 9"]).is_err());
        assert!(optimum_result_table_from_rows(&["N NT 14"]).is_err());
    }

    #[test]
    fn row_recognition_separates_data_from_everything_else() {
        assert!(is_optimum_result_row("N NT  9"));
        assert!(is_optimum_result_row("W  C 13"));
        assert!(!is_optimum_result_row("[Board \"1\"]"));
        assert!(!is_optimum_result_row(""));
        assert!(!is_optimum_result_row("N NT 14"));
        assert!(!is_optimum_result_row("1NT Pass 3NT AP"));
    }

    /// A real tag, against the table the solver independently reported for the
    /// same deal.
    ///
    /// `N:62.JT765.AKJ5.Q3 KQ85.Q9.Q876.J75 J9743.K84.T2.K84 AT.A32.943.AT962`
    /// annotated by the `bridge-solver` CLI produced this `DoubleDummyTricks`
    /// value, and `solver-diag` printed the same twenty cells for it. This
    /// pins the orderings against something outside the crate: transpose the
    /// rows or start the columns at clubs and it fails.
    #[test]
    fn decodes_a_real_bridge_composer_value() {
        let table = dd_table_from_pbn("56865568656757867578").unwrap();

        // North and South hold the same cards' worth here; East-West likewise.
        for declarer in [Direction::North, Direction::South] {
            assert_eq!(table.tricks(declarer, Strain::NoTrump), 5);
            assert_eq!(table.tricks(declarer, Strain::Spades), 6);
            assert_eq!(table.tricks(declarer, Strain::Hearts), 8);
            assert_eq!(table.tricks(declarer, Strain::Diamonds), 6);
            assert_eq!(table.tricks(declarer, Strain::Clubs), 5);
        }
        for declarer in [Direction::East, Direction::West] {
            assert_eq!(table.tricks(declarer, Strain::NoTrump), 6);
            assert_eq!(table.tricks(declarer, Strain::Spades), 7);
            assert_eq!(table.tricks(declarer, Strain::Hearts), 5);
            assert_eq!(table.tricks(declarer, Strain::Diamonds), 7);
            assert_eq!(table.tricks(declarer, Strain::Clubs), 8);
        }

        assert_eq!(dd_table_to_pbn(&table), "56865568656757867578");
    }

    /// The tag survives a read/write cycle through `Board`'s typed field.
    #[test]
    fn the_tag_round_trips_through_a_board() {
        use crate::pbn::{read_pbn, write_pbn};

        let src = concat!(
            "[Board \"1\"]\n",
            "[Deal \"N:62.JT765.AKJ5.Q3 KQ85.Q9.Q876.J75 J9743.K84.T2.K84 AT.A32.943.AT962\"]\n",
            "[DoubleDummyTricks \"56865568656757867578\"]\n",
        );
        let boards = read_pbn(src).unwrap();
        assert_eq!(
            boards[0]
                .double_dummy_tricks
                .unwrap()
                .tricks(Direction::North, Strain::Hearts),
            8
        );
        assert!(write_pbn(&boards).contains("[DoubleDummyTricks \"56865568656757867578\"]"));
    }

    /// A value we cannot read is dropped, not re-emitted as though it were
    /// analysis.
    #[test]
    fn an_unreadable_tag_does_not_survive_as_corruption() {
        use crate::pbn::{read_pbn, write_pbn};

        let src = concat!(
            "[Board \"1\"]\n",
            "[Deal \"N:62.JT765.AKJ5.Q3 KQ85.Q9.Q876.J75 J9743.K84.T2.K84 AT.A32.943.AT962\"]\n",
            "[DoubleDummyTricks \"nonsense\"]\n",
        );
        let boards = read_pbn(src).unwrap();
        assert!(boards[0].double_dummy_tricks.is_none());
        assert!(!write_pbn(&boards).contains("DoubleDummyTricks"));
    }

    /// The two encodings must describe the same table.
    #[test]
    fn the_two_encodings_agree() {
        let table = distinct_table();
        let from_string = dd_table_from_pbn(&dd_table_to_pbn(&table)).unwrap();
        let from_rows = optimum_result_table_from_rows(&optimum_result_table_rows(&table)).unwrap();
        assert_eq!(from_string, from_rows);
    }
}
