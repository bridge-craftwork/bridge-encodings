//! Streaming deal reader with format auto-detection.
//!
//! Reads deals from any `BufRead` source, auto-detecting PBN, oneline,
//! and printall formats. Non-deal lines (PBN metadata, blank lines,
//! statistics output) are silently skipped.
//!
//! # Example
//!
//! ```
//! use bridge_encodings::DealReader;
//! use std::io::Cursor;
//!
//! let input = "n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72\n";
//! let reader = DealReader::new(Cursor::new(input));
//! let deals: Vec<_> = reader.collect();
//! assert_eq!(deals.len(), 1);
//! ```

use crate::error::{ParseError, Result};
use bridge_types::Deal;
use std::io::BufRead;

/// Reads deals from a text source (file, stdin, network stream, etc.).
///
/// Supports PBN, oneline, and printall formats with auto-detection.
/// Non-deal lines are silently skipped, making it safe to feed raw
/// dealer.exe output (which includes statistics lines) directly.
pub struct DealReader<R: BufRead> {
    reader: R,
    line_buf: String,
    line_number: usize,
    deals_read: usize,
}

impl<R: BufRead> DealReader<R> {
    /// Create a new reader with auto-detection.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line_buf: String::new(),
            line_number: 0,
            deals_read: 0,
        }
    }

    /// Number of deals successfully read so far.
    pub fn deals_read(&self) -> usize {
        self.deals_read
    }

    /// Current line number in the input.
    pub fn line_number(&self) -> usize {
        self.line_number
    }

    /// Read one line from the underlying reader. Returns false at EOF.
    fn read_line(&mut self) -> std::result::Result<bool, std::io::Error> {
        self.line_buf.clear();
        match self.reader.read_line(&mut self.line_buf) {
            Ok(0) => Ok(false),
            Ok(_) => {
                self.line_number += 1;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    /// Try to parse the next 4 lines as a printall suit block.
    /// Called when we've already seen a board number header line.
    fn try_read_printall(&mut self) -> Option<Result<Deal>> {
        let mut suit_lines = Vec::with_capacity(4);

        for _ in 0..4 {
            match self.read_line() {
                Ok(true) => suit_lines.push(self.line_buf.clone()),
                Ok(false) => return None,
                Err(e) => return Some(Err(ParseError::Io(e))),
            }
        }

        // Build the lines slice for parse_printall (header + 4 suit lines)
        let header = "   1.\n".to_string(); // Dummy header - parse_printall just validates format
        let all_lines: Vec<&str> = std::iter::once(header.as_str())
            .chain(suit_lines.iter().map(|s| s.as_str()))
            .collect();

        match crate::printall::parse_printall(&all_lines) {
            Ok((deal, _)) => {
                self.deals_read += 1;
                Some(Ok(deal))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

/// Check if a line looks like a printall board number header (e.g. "   1.", "  42.")
fn is_board_number_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.ends_with('.')
        && !trimmed.is_empty()
        && trimmed[..trimmed.len() - 1].trim().parse::<usize>().is_ok()
}

impl<R: BufRead> Iterator for DealReader<R> {
    type Item = Result<Deal>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.read_line() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => return Some(Err(ParseError::Io(e))),
            }

            let line = self.line_buf.trim().to_string();

            if line.is_empty() {
                continue;
            }

            // Try oneline format first (cheap check: 8 whitespace-separated parts)
            if let Ok(deal) = crate::oneline::parse_oneline(&line) {
                self.deals_read += 1;
                return Some(Ok(deal));
            }

            // Try PBN Deal tag: [Deal "N:..."]
            if line.starts_with("[Deal ") {
                if let Some(deal) = try_parse_pbn_deal_tag(&line) {
                    self.deals_read += 1;
                    return Some(Ok(deal));
                }
            }

            // Try printall: board number header followed by 4 suit lines
            if is_board_number_line(&line) {
                if let Some(result) = self.try_read_printall() {
                    return Some(result);
                }
            }

            // Unrecognized line — skip (PBN metadata, stats, comments, etc.)
        }
    }
}

/// Extract and parse the deal value from a PBN Deal tag line.
fn try_parse_pbn_deal_tag(line: &str) -> Option<Deal> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let rest = inner.strip_prefix("Deal ")?;
    let value = rest.strip_prefix('"')?.strip_suffix('"')?;
    Deal::from_pbn(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_types::Direction;
    use std::io::Cursor;

    #[test]
    fn test_read_oneline_deals() {
        let input = "\
n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72
n A754.7642.KJ2.A9 e QT.AK95.87.K8652 s K93.J83.QT6543.T w J862.QT.A9.QJ743
";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 2);
        assert!(deals.iter().all(|r| r.is_ok()));
    }

    #[test]
    fn test_read_pbn_deals() {
        let input = r#"[Event "test"]
[Board "1"]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:KQ4.QJ982..AKQ43 J653.A73.985.J97 9.K54.KQT732.652 AT872.T6.AJ64.T8"]
[Result "?"]

[Event "test"]
[Board "2"]
[Deal "N:AQ62.942.KQ.AJ64 73.7.J8742.KQ532 KJ54.QJ3.653.T98 T98.AKT865.AT9.7"]
"#;
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 2);
        assert!(deals.iter().all(|r| r.is_ok()));
    }

    #[test]
    fn test_auto_detect_mixed_formats() {
        let input = "\
n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72
[Deal \"N:AQ62.942.KQ.AJ64 73.7.J8742.KQ532 KJ54.QJ3.653.T98 T98.AKT865.AT9.7\"]
";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 2);
        assert!(deals.iter().all(|r| r.is_ok()));
    }

    #[test]
    fn test_skip_non_deal_lines() {
        let input = "\
Generated 100 hands
Produced 5 hands
n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72
Initial random seed 42
Time needed    0.123 sec
";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 1);
        assert!(deals[0].is_ok());
    }

    #[test]
    fn test_empty_input() {
        let reader = DealReader::new(Cursor::new(""));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 0);
    }

    #[test]
    fn test_deals_read_counter() {
        let input = "\
n AKQT3.J6.KJ42.95 e 652.AK42.AQ87.T4 s J74.QT95.T.AK863 w 98.873.9653.QJ72
n A754.7642.KJ2.A9 e QT.AK95.87.K8652 s K93.J83.QT6543.T w J862.QT.A9.QJ743
";
        let mut reader = DealReader::new(Cursor::new(input));
        reader.next();
        assert_eq!(reader.deals_read(), 1);
        assert_eq!(reader.line_number(), 1);
        reader.next();
        assert_eq!(reader.deals_read(), 2);
        assert_eq!(reader.line_number(), 2);
    }

    #[test]
    fn test_pbn_with_metadata_skipped() {
        let input = r#"% PBN 2.1
% EXPORT

[Event ""]
[Site ""]
[Date ""]
[Board "1"]
[West ""]
[North ""]
[East ""]
[South ""]
[Dealer "N"]
[Vulnerable "None"]
[Deal "N:KQ4.QJ982..AKQ43 J653.A73.985.J97 9.K54.KQT732.652 AT872.T6.AJ64.T8"]
[Scoring ""]
[Declarer ""]
[Contract ""]
[Result ""]
"#;
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 1);
        let deal = deals[0].as_ref().unwrap();
        assert_eq!(deal.hand(Direction::North).len(), 13);
    }

    #[test]
    fn test_dealer_exe_full_output() {
        // Simulated dealer.exe output with Fn prefix on oneline format
        let input = "\
Fn AKT43.AJ9532.Q.2 e Q75.QT6.T74.T964 s J8..AK653.AJ8753 w 962.K874.J982.KQ
n 9.2.AKT985.AKQ92 e 8652.A53.J43.JT5 s AKQJT4.QJ974.6.6 w 73.KT86.Q72.8743
Generated 534652 hands
Produced 10 hands
Initial random seed 1771167619
Time needed    0.996 sec
";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        // The "Fn" line won't parse as oneline (F isn't a direction), so only 1 deal
        assert_eq!(deals.len(), 1);
    }

    #[test]
    fn test_read_printall_format() {
        let input = "\
   1.
J 7 3               9 8                 A Q 5 4 2           K T 6
3                   9 6 4 2             K J 8 7             A Q T 5
K Q J T 9 8 5       7                   3 2                 A 6 4
T 5                 9 8 7 4 3 2         A K                 Q J 6

";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 1);
        let deal = deals[0].as_ref().unwrap();
        assert_eq!(deal.hand(Direction::North).len(), 13);
        assert_eq!(deal.hand(Direction::East).len(), 13);
        assert_eq!(deal.hand(Direction::South).len(), 13);
        assert_eq!(deal.hand(Direction::West).len(), 13);
    }

    #[test]
    fn test_read_printall_with_stats() {
        let input = "\
   1.
J 7 3               9 8                 A Q 5 4 2           K T 6
3                   9 6 4 2             K J 8 7             A Q T 5
K Q J T 9 8 5       7                   3 2                 A 6 4
T 5                 9 8 7 4 3 2         A K                 Q J 6

Generated 100 hands
Produced 1 hands
Initial random seed 42
Time needed    0.001 sec
";
        let reader = DealReader::new(Cursor::new(input));
        let deals: Vec<_> = reader.collect();
        assert_eq!(deals.len(), 1);
    }
}
