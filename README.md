# bridge-encodings

File format parsers and writers for contract bridge in Rust.

## Overview

`bridge-encodings` provides parsers and writers for common bridge file formats. It builds on [`bridge-types`](https://github.com/bridge-craftwork/bridge-types) for core data structures.

## Supported Formats

| Format | Read | Write | Description |
|--------|------|-------|-------------|
| **PBN** | Yes | Yes | Portable Bridge Notation - standard interchange format |
| **LIN** | Yes | Yes | Bridge Base Online hand records |
| **Oneline** | Yes | Yes | Simple format used by dealer.exe |

## Installation

```toml
[dependencies]
bridge-encodings = { git = "https://github.com/bridge-craftwork/bridge-encodings" }
```

## Quick Start

### Reading PBN Files

```rust
use bridge_encodings::pbn;

let pbn_content = r#"
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ"]
"#;

let boards = pbn::read_pbn(pbn_content).unwrap();
println!("Loaded {} boards", boards.len());
```

### Editing PBN Files In Place

`read_pbn` + `write_pbn` re-emit from a typed model, so whitespace, tag order and
`%` directives do not survive. To *touch up* a file a human authored — change or
add a tag and leave every other byte exactly as written — use `PbnDocument`,
which holds the original text and an index into it:

```rust
use bridge_encodings::pbn::PbnDocument;

let mut doc = PbnDocument::parse_file(path)?;
for i in 0..doc.boards().len() {
    doc.set_tag(i, "DoubleDummyTricks", &ddt)?;
    doc.set_section(i, "OptimumResultTable", "Declarer;Result", &rows)?;
}
if doc.is_modified() {
    doc.write_file(path)?;  // untouched boards keep their original bytes
}
```

An unedited document round-trips byte-for-byte — CRLF, mixed endings, a missing
final newline, `%` directives, `;` comments and `{...}` commentary all included —
and inserted lines take the line ending the surrounding file uses. Setting a tag
to the value it already holds leaves `is_modified()` false, so repeated runs over
a tree rewrite nothing.

Two things a tag name alone cannot address have their own calls. `Note` is the
one tag the standard lets repeat (PBN 2.1 §3.5.5), so `set_tag` — which replaces
the first tag of a name — cannot write a second note; `set_tags` replaces the
whole run instead. And program-written commentary such as `{HCP 4 16 9 11}` is
found by its leading keyword, so a re-run replaces it rather than adding a copy:

```rust
// Re-bidding a board: its auction, every note, and the deal commentary are
// replaced; tags the program does not own are left exactly as they were.
doc.set_section(i, "Auction", "N", &["Pass 1C =1= 1H 4H", "AP"])?;
doc.set_tags(i, "Note", &["1:Precision"])?;   // no stale notes survive; &[] clears them
doc.set_comment(i, "Deal", "HCP", "4 16 9 11")?; // after [Deal], or replaced in place
```

A `%` directive with no keyword — BBA's 28-hex board fingerprint, say — is found
by a predicate instead. `set_directive` refuses text its own predicate would not
recognise, since that line would be invisible to the next run and get duplicated:

```rust
let is_hash = |t: &str| t.len() == 28 && t.chars().all(|c| c.is_ascii_hexdigit());
doc.set_directive(i, "Board", &hash, is_hash)?; // after [Board], or replaced in place
```

### Writing PBN Files

```rust
use bridge_encodings::pbn;
use bridge_types::Board;

let boards: Vec<Board> = vec![/* ... */];
let pbn_output = pbn::write_pbn(&boards);
```

### Reading and Writing LIN

```rust
use bridge_encodings::{lin, pbn};

// Boards: one record per board, laid out the way Bridge Composer exports LIN.
let boards = lin::parse_lin_file(lin_content).unwrap();
let records: Vec<lin::LinData> = pbn::read_pbn(pbn_content)
    .unwrap()
    .iter()
    .map(lin::LinData::from_board)
    .collect();
let lin_output = lin::write_lin_file(&records);

// Any LIN text, teaching movies included, token by token and back unchanged.
let doc = lin::LinDocument::parse(movie);
assert_eq!(doc.to_lin(), movie);
```

What Bridge Composer does with LIN, and where this crate departs from it, is
recorded in [`fixtures/lin/README.md`](fixtures/lin/README.md).

### Oneline Format

```rust
use bridge_encodings::oneline;
use bridge_types::Deal;

// Parse
let deal = oneline::parse_oneline("N:AKQ.xxx.xxx.xxxx ...").unwrap();

// Generate
let output = oneline::deal_to_oneline(&deal, bridge_types::Direction::North);
```

## PBN Format Details

The PBN (Portable Bridge Notation) format is the standard for bridge data interchange. This crate supports:

### Mandatory Tags (read/write)
- `Board` - Board number
- `Dealer` - Dealer position (N/E/S/W)
- `Vulnerable` - Vulnerability (None/NS/EW/Both)
- `Deal` - Card distribution

### Supplemental Tags (read/write)
- `Event`, `Site`, `Date` - Tournament info
- `Declarer`, `Contract`, `Result` - Outcome
- `North`, `East`, `South`, `West` - Player names
- `DoubleDummyTricks` - DD analysis results
- `OptimumScore`, `ParContract` - Par calculation
- Any other tag - preserved verbatim in `Board::extra_tags`, in encounter order

### Sections (read/write)
- `Auction` - parsed into a typed `Auction`, with `Note` annotations
- `Play` - parsed into a typed `PlaySequence`
- `{...}` commentary blocks - preserved in `Board::commentary`

### Directives and Comments

`%` directives and `;` comments are preserved, not dropped. `%` is where Bridge
Composer keeps a file's fonts, page setup and colours, so discarding them would
silently strip a user's page layout. Each one rides on the board whose record it
sits in, anchored to the tag it followed, and `write_pbn` puts it back there.

The one case a `Vec<Board>` cannot carry is a file with no board records at all —
a header-only template has no board to hang its directives on. Use `PbnDocument`
for that, and whenever the file's exact bytes matter.

## PBN Specification

This crate follows the [Portable Bridge Notation v2.1 specification](https://www.tistis.nl/pbn/pbn_v21.txt).

A copy of the specification is included in [docs/pbn_v21.txt](docs/pbn_v21.txt) for reference.

## Error Handling

```rust
use bridge_encodings::{pbn, ParseError, Result};

fn load_file(content: &str) -> Result<()> {
    let boards = pbn::read_pbn(content)?;
    // ...
    Ok(())
}
```

## Re-exports

For convenience, core types from `bridge-types` are re-exported:

```rust
use bridge_encodings::{Board, Card, Deal, Direction, Hand, Suit, Rank, Vulnerability};
```

## Related Crates

- [`bridge-types`](https://github.com/bridge-craftwork/bridge-types) - Core data types (dependency)
- [`bridge-solver`](https://github.com/bridge-craftwork/bridge-solver) - Double-dummy analysis
- [`pbn-to-pdf`](https://github.com/bridge-craftwork/pbn-to-pdf) - PDF generation from PBN

## License

This project is in the public domain (Unlicense).
