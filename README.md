# bridge-encodings

File format parsers and writers for contract bridge in Rust.

## Overview

`bridge-encodings` provides parsers and writers for common bridge file formats. It builds on [`bridge-types`](https://github.com/bridge-craftwork/bridge-types) for core data structures.

## Supported Formats

| Format | Read | Write | Description |
|--------|------|-------|-------------|
| **PBN** | Yes | Yes | Portable Bridge Notation - standard interchange format |
| **LIN** | Yes | No | Bridge Base Online hand records |
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

### Writing PBN Files

```rust
use bridge_encodings::pbn;
use bridge_types::Board;

let boards: Vec<Board> = vec![/* ... */];
let pbn_output = pbn::write_pbn(&boards);
```

### Reading LIN Files

```rust
use bridge_encodings::lin;

let lin_content = "pn|North,East,South,West|st||md|...";
let boards = lin::read_lin(lin_content).unwrap();
```

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
- `DoubleDummyTricks` - DD analysis results
- `OptimumScore`, `ParContract` - Par calculation

### Not Yet Implemented
- `Auction` section
- `Play` section
- `Result` tag
- Player name tags

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
