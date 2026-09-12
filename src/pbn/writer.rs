//! PBN file writer.
//!
//! Re-emits the `%` directives and `;` comments a board preserved, each after
//! the tag it followed in the source, so a read/write cycle does not strip a
//! file's page setup or a hand author's notes.

use std::collections::HashSet;

use bridge_types::{Auction, Board, Call, Card, Direction, PlaySequence};

/// Write boards to PBN format
pub fn write_pbn(boards: &[Board]) -> String {
    let mut output = String::new();

    // PBN header. A file read in with its own header keeps that one — the
    // first board carries it as leading directives — rather than gaining a
    // second, invented one on top.
    let has_own_header = boards
        .first()
        .is_some_and(|b| b.leading_directives().next().is_some());
    if !has_own_header {
        output.push_str("% PBN 2.1\n");
        output.push_str("% EXPORT\n");
        output.push('\n');
    }

    for (i, board) in boards.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        // A file's own header is separated from the first record by a blank
        // line, the way the invented one above is — so re-reading what we write
        // finds the same header again and the next cycle changes nothing.
        let (leading, lines) = board_lines(board);
        let blank_after = if i == 0 { leading } else { 0 };
        for (n, line) in lines.iter().enumerate() {
            if n == blank_after && blank_after > 0 {
                output.push('\n');
            }
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

/// Convert a single board to PBN format
///
/// Preserved `%` directives and `;` comments are re-emitted after the tag each
/// one followed, so they keep the place their author gave them even though this
/// writer chooses its own tag order. One anchored to a tag this writer does not
/// emit is appended rather than dropped.
pub fn board_to_pbn(board: &Board) -> String {
    let (_, lines) = board_lines(board);
    lines.join("\n") + "\n"
}

/// A board's lines, with the count of leading ones that are directives from
/// before its first tag — the file header, on the first board of a file.
fn board_lines(board: &Board) -> (usize, Vec<String>) {
    let mut out = TagWriter::new(board);

    out.tag("Event", board.event.as_deref().unwrap_or(""));
    out.tag("Site", board.site.as_deref().unwrap_or(""));
    out.tag("Date", board.date.as_deref().unwrap_or(""));

    // Board identifier — prefer the raw id (preserves "1-1"), else the number.
    if let Some(ref id) = board.board_id {
        out.tag("Board", id);
    } else if let Some(num) = board.number {
        out.tag("Board", &num.to_string());
    }

    // Player names (preserve when present, else empty for hand records)
    for dir in [
        Direction::West,
        Direction::North,
        Direction::East,
        Direction::South,
    ] {
        let name = board
            .player_names
            .as_ref()
            .and_then(|p| p.get(dir))
            .unwrap_or("");
        out.tag(direction_tag(dir), name);
    }

    if let Some(dealer) = board.dealer {
        out.tag("Dealer", &dealer.to_char().to_string());
    }

    out.tag("Vulnerable", board.vulnerable.to_pbn());

    // Supplemental tags that came before the deal in the source go back before
    // it. Where a tag stands decides the slot of the commentary that follows
    // it: Bridge Composer never prints a block between [Board] and [Deal], so
    // writing the tag after the deal would bring the block into view.
    // `Scoring` keeps its dedicated line below.
    let deal_at = board.tag_order.iter().position(|t| t == "Deal");
    for (name, value) in &board.extra_tags {
        let before_deal = deal_at.is_some_and(|at| board.tag_order[..at].contains(name));
        if before_deal && name != "Scoring" && !out.already_wrote(name) {
            out.tag(name, value);
        }
    }

    // A deal that did not parse goes back as it was written, rather than as
    // the empty deal it parsed to
    match board.unparsed_deal.as_deref() {
        Some(text) => out.tag("Deal", text),
        None => {
            let first_dir = board.dealer.unwrap_or(Direction::North);
            out.tag("Deal", &board.deal.to_pbn(first_dir));
        }
    }

    // Scoring / result block — preserved when present. `Scoring` has no
    // dedicated field, so a value read from a file arrives in `extra_tags`;
    // take it from there rather than writing a second, empty one.
    out.tag("Scoring", board.extra_tag("Scoring").unwrap_or(""));
    out.tag(
        "Declarer",
        &board
            .declarer
            .map(|d| d.to_char().to_string())
            .unwrap_or_default(),
    );
    out.tag("Contract", board.contract.as_deref().unwrap_or(""));
    out.tag(
        "Result",
        &board.result.map(|r| r.to_string()).unwrap_or_default(),
    );

    // Analysis tags if present
    if let Some(ref dd) = board.double_dummy_tricks {
        out.tag("DoubleDummyTricks", &super::dd_table_to_pbn(dd));
    }
    if let Some(ref opt) = board.optimum_score {
        out.tag("OptimumScore", opt);
    }
    if let Some(ref par) = board.par_contract {
        out.tag("ParContract", par);
    }

    // Supplemental / custom tags, preserved verbatim in encounter order. One
    // this writer has already emitted from a dedicated line is skipped, or a
    // read/write cycle would grow a duplicate of it every time round.
    for (name, value) in &board.extra_tags {
        if !out.already_wrote(name) {
            out.tag(name, value);
        }
    }

    // Auction section, followed by the notes its calls refer to. A `=n=`
    // annotation without its `[Note "n:..."]` is a dangling reference, so the
    // two are written together or not at all.
    if let Some(ref auction) = board.auction {
        out.section(
            "Auction",
            &auction.dealer.to_char().to_string(),
            auction_lines(auction),
        );
        let mut numbers: Vec<_> = auction.notes.keys().copied().collect();
        numbers.sort_unstable();
        for n in numbers {
            if let Some(text) = auction.get_note(n) {
                out.tag("Note", &format!("{n}:{text}"));
            }
        }
    }

    // Play section.
    if let Some(ref play) = board.play {
        out.section(
            "Play",
            &play.opening_leader.to_char().to_string(),
            play_lines(play),
        );
    }

    out.finish()
}

/// Assembles a board's lines, keeping each preserved directive with the tag it
/// followed and making sure none is silently left out.
struct TagWriter<'a> {
    board: &'a Board,
    lines: Vec<String>,
    /// Tags actually written, so `finish` can tell which anchors never came up.
    emitted: HashSet<String>,
    /// How many lines the board opened with before any tag.
    leading: usize,
    /// Which commentary blocks have been written, so each goes out once.
    placed: Vec<bool>,
}

impl<'a> TagWriter<'a> {
    /// Start a board, led by the directives that preceded every tag in its
    /// source record.
    fn new(board: &'a Board) -> Self {
        let lines: Vec<String> = board.leading_directives().map(str::to_string).collect();
        let mut out = Self {
            board,
            leading: lines.len(),
            lines,
            emitted: HashSet::new(),
            placed: vec![false; board.commentary.len()],
        };
        // Commentary from before every tag is board content, not file header,
        // so it follows the header's directives without being counted in them.
        out.push_commentary(|anchor, _| anchor == Some(0));
        out
    }

    /// Write a tag, followed by the directives the board kept after it.
    fn tag(&mut self, name: &str, value: &str) {
        self.lines.push(format!("[{} \"{}\"]", name, value));
        self.push_directives_after(name);
    }

    /// Write a section tag and its data lines. Directives anchored to the tag
    /// follow the data, so the calls or cards stay contiguous under it.
    fn section(&mut self, name: &str, value: &str, data: Vec<String>) {
        self.lines.push(format!("[{} \"{}\"]", name, value));
        self.lines.extend(data);
        self.push_directives_after(name);
    }

    /// Whether a tag of this name has been written already.
    fn already_wrote(&self, name: &str) -> bool {
        self.emitted.contains(name)
    }

    fn push_directives_after(&mut self, name: &str) {
        let board = self.board;
        self.lines
            .extend(board.directives_after(name).map(str::to_string));
        // A block goes back after the tag it followed, and only after the
        // first one of that name: a repeated tag such as `Note` would
        // otherwise write it more than once.
        if self.emitted.insert(name.to_string()) {
            self.push_commentary(|_, tag| tag == Some(name));
        }
    }

    /// Write, in order, the commentary blocks not yet written whose anchor
    /// (see `Board::commentary_anchors`) and anchoring tag satisfy `wanted`.
    fn push_commentary(&mut self, wanted: impl Fn(Option<usize>, Option<&str>) -> bool) {
        let board = self.board;
        for (i, block) in board.commentary.iter().enumerate() {
            let anchor = board.commentary_anchors.get(i).copied();
            if !self.placed[i] && wanted(anchor, board.commentary_anchor_tag(i)) {
                self.lines.push(format!("{{{}}}", block));
                self.placed[i] = true;
            }
        }
    }

    /// Finish the board, appending any directive whose tag this writer never
    /// emitted — the file had that line, so it goes back somewhere.
    fn finish(mut self) -> (usize, Vec<String>) {
        // Commentary whose place is unknown, or whose tag was never written,
        // goes at the end, which is where every block went before anchors.
        self.push_commentary(|_, _| true);
        let orphans: Vec<String> = self
            .board
            .directives
            .iter()
            .filter(|d| {
                d.after_tag
                    .as_deref()
                    .is_some_and(|tag| !self.emitted.contains(tag))
            })
            .map(|d| d.text.clone())
            .collect();
        self.lines.extend(orphans);
        (self.leading, self.lines)
    }
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

/// Format an auction's calls, four per line (one bidding round per line),
/// closed by its end marker when it has one.
///
/// An annotation is written glued to its call (`2H=1=`), which is how the
/// reader took it in and the form that cannot be misread: a space would leave
/// it looking like a token of its own, which is legal but reads as annotating
/// whatever it happens to follow after a line wrap.
fn auction_lines(auction: &Auction) -> Vec<String> {
    let mut lines: Vec<String> = auction
        .calls
        .chunks(4)
        .map(|round| {
            round
                .iter()
                .map(|c| match c.annotation {
                    Some(ref ann) => format!("{}{}", c.call.to_pbn(), ann),
                    None => c.call.to_pbn(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    // A `Call::Continue` already writes the `+`; adding the end marker too
    // would write it twice.
    let marker_is_a_call = matches!(auction.calls.last().map(|c| &c.call), Some(Call::Continue));
    if !marker_is_a_call {
        lines.extend(auction.end.to_pbn().map(str::to_string));
    }
    lines
}

/// Format a play sequence, one trick per line in play order, closed by its end
/// marker when it has one.
fn play_lines(play: &PlaySequence) -> Vec<String> {
    let mut lines: Vec<String> = play
        .tricks
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
        .collect();
    lines.extend(play.end.to_pbn().map(str::to_string));
    lines
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

    #[test]
    fn a_read_write_cycle_keeps_directives_in_place() {
        use crate::pbn::read_pbn;

        let pbn = concat!(
            "% PBN 2.1\n",
            "% Creator \"Bridge Composer\"\n",
            "\n",
            "[Board \"1\"]\n",
            "% 065A62DCF61869AE5D72DF8D408A\n",
            "; checked by hand\n",
            "[Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n",
        );
        let out = write_pbn(&read_pbn(pbn).unwrap());

        // Every directive survives, each still after the tag it followed.
        assert!(out.starts_with("% PBN 2.1\n% Creator \"Bridge Composer\"\n"));
        assert!(out.contains("[Board \"1\"]\n% 065A62DCF61869AE5D72DF8D408A\n; checked by hand\n"));

        // And a second cycle is stable.
        let again = write_pbn(&read_pbn(&out).unwrap());
        assert_eq!(again, out);
    }

    #[test]
    fn a_file_with_its_own_header_does_not_gain_an_invented_one() {
        use crate::pbn::read_pbn;

        let pbn = "% Bridge Composer 5.9\n\n[Board \"1\"]\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(out.starts_with("% Bridge Composer 5.9\n"));
        assert!(!out.contains("% PBN 2.1"));
        assert!(!out.contains("% EXPORT"));

        // A file with no header of its own still gets the default one.
        let out = write_pbn(&read_pbn("[Board \"1\"]\n").unwrap());
        assert!(out.starts_with("% PBN 2.1\n% EXPORT\n\n"));
    }

    #[test]
    fn a_directive_anchored_to_an_unwritten_tag_is_still_written() {
        // `Note` is re-emitted only from an auction's notes, so on a board
        // with no auction its anchor never comes up. The line still has to
        // land somewhere.
        let board = Board::new()
            .with_number(1)
            .with_directive("% orphaned by the writer", Some("Note"));
        let pbn = board_to_pbn(&board);
        assert!(pbn.contains("% orphaned by the writer"));
    }

    #[test]
    fn a_tag_the_writer_emits_itself_does_not_double_on_a_round_trip() {
        use crate::pbn::read_pbn;

        // `Scoring` has no dedicated field, so it comes back in extra_tags
        // alongside the line the writer always emits. Cycling must not stack up
        // copies of it, nor lose the value it carried.
        let pbn = "[Board \"1\"]\n[Scoring \"IMP\"]\n";
        let mut out = write_pbn(&read_pbn(pbn).unwrap());
        for _ in 0..3 {
            assert_eq!(out.matches("[Scoring ").count(), 1, "in:\n{out}");
            assert!(out.contains("[Scoring \"IMP\"]"));
            out = write_pbn(&read_pbn(&out).unwrap());
        }
    }

    #[test]
    fn a_section_that_is_only_a_marker_survives_a_round_trip() {
        use crate::pbn::read_pbn;

        // Previously this wrote back as a bare [Play "W"], which then read as
        // nothing at all — the section and its opening leader both gone.
        let pbn = "[Board \"1\"]\n[Play \"W\"]\n*\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(out.contains("[Play \"W\"]\n*\n"), "in:\n{out}");

        // And it is stable: cycling again changes nothing.
        assert_eq!(write_pbn(&read_pbn(&out).unwrap()), out);
    }

    #[test]
    fn an_auction_keeps_its_closing_marker() {
        use crate::pbn::read_pbn;

        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT Pass\n*\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(out.contains("[Auction \"N\"]\n1NT Pass\n*\n"), "in:\n{out}");
        // Notrump is written "NT", per 3.4.14 and 3.5.1.
        assert!(!out.contains("1N Pass"));
        assert_eq!(write_pbn(&read_pbn(&out).unwrap()), out);

        // An unmarked auction gains no marker.
        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT Pass Pass Pass\n";
        assert!(!write_pbn(&read_pbn(pbn).unwrap()).contains('*'));
    }

    #[test]
    fn an_annotated_call_and_its_note_round_trip_together() {
        use crate::pbn::read_pbn;

        let pbn = "[Board \"1\"]\n[Auction \"N\"]\n1NT=1= Pass\n[Note \"1:15-17 balanced\"]\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(out.contains("1NT=1= Pass"), "annotation is kept:\n{out}");
        assert!(
            out.contains("[Note \"1:15-17 balanced\"]"),
            "the note it refers to is written too, or the reference dangles:\n{out}"
        );
        assert_eq!(
            write_pbn(&read_pbn(&out).unwrap()),
            out,
            "stable on re-read"
        );
    }

    #[test]
    fn ap_is_written_as_the_passes_it_meant() {
        use crate::pbn::read_pbn;

        // "AP" is read as three passes, so that is what comes back out. The
        // file changes shape; the auction it describes does not.
        let out =
            write_pbn(&read_pbn("[Board \"1\"]\n[Auction \"N\"]\n1NT Pass 3NT AP\n").unwrap());
        assert!(out.contains("1NT Pass 3NT Pass\nPass Pass"), "in:\n{out}");
        assert_eq!(write_pbn(&read_pbn(&out).unwrap()), out);
    }

    #[test]
    fn commentary_goes_back_after_the_tag_it_followed() {
        use crate::pbn::read_pbn;
        // Moving a block changes what it means to Bridge Composer: one between
        // [Board] and [Deal] is never printed, one after [Result] is. Writing
        // every block at the end, as this used to, turned the first into the
        // second.
        let pbn = "{Leading}\n[Event \"E\"]\n[Board \"1\"]\n[SkillPath \"x\"]\n\
                   {Before the deal}\n\
                   [Deal \"N:K843.T542.J6.863 AQJ7.K.Q75.AT942 962.AJ7.KT82.J75 T5.Q9863.A943.KQ\"]\n\
                   [Result \"10\"]\n{After the result}\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        let again = &read_pbn(&out).unwrap()[0];
        let tags: Vec<_> = (0..3).map(|i| again.commentary_anchor_tag(i)).collect();
        assert_eq!(
            tags,
            [None, Some("SkillPath"), Some("Result")],
            "in:\n{out}"
        );
        let before = out.find("{Before the deal}").unwrap();
        assert!(
            before < out.find("[Deal ").unwrap(),
            "moved past the deal:\n{out}"
        );
    }

    #[test]
    fn a_deal_that_did_not_parse_is_written_back_as_it_was() {
        use crate::pbn::read_pbn;
        let pbn = "[Board \"1\"]\n[Deal \"W:Qx.QJ9xxxx.K.xxx ... ... ...\"]\n";
        let out = write_pbn(&read_pbn(pbn).unwrap());
        assert!(
            out.contains("[Deal \"W:Qx.QJ9xxxx.K.xxx ... ... ...\"]"),
            "in:\n{out}"
        );
    }
}
