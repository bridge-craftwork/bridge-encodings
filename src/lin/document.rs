//! LIN as its `command|value|` tokens, kept exactly as written.
//!
//! The board-level reader keeps only what a board needs. Everything else a LIN
//! text can carry — commentary (`at`, `nt`), page breaks (`pg`), highlights
//! (`hs`, `hc`), a teaching movie's rewinds (`up`) and re-deals (`md|0…|`),
//! commands this crate has never heard of — lives here, so a file can be read
//! and written back without losing a byte.

/// One `command|value|` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinToken {
    /// The command as written, with any whitespace around it: a line break
    /// between boards, or the blank lines a movie puts before a page break.
    pub raw_command: String,
    /// Everything between the command's pipe and the next, verbatim.
    pub value: String,
}

impl LinToken {
    /// A token with nothing around its command.
    pub fn new(command: &str, value: &str) -> Self {
        Self {
            raw_command: command.to_string(),
            value: value.to_string(),
        }
    }

    /// The command name without surrounding whitespace, in the case it was
    /// written (`md`, `pc`, and in some movies `At`).
    pub fn command(&self) -> &str {
        self.raw_command.trim()
    }

    /// Whether this is the given command, ignoring case.
    pub fn is(&self, command: &str) -> bool {
        self.command().eq_ignore_ascii_case(command)
    }
}

/// A LIN text as a sequence of tokens that writes back exactly as it was read.
///
/// ```
/// use bridge_encodings::lin::LinDocument;
///
/// let movie = "st||md|3SAK,SQJ,ST9|at|Lead a spade|\n\npg||pc|s|up|1|";
/// let doc = LinDocument::parse(movie);
/// assert_eq!(doc.tokens[2].command(), "at");
/// assert_eq!(doc.tokens[3].command(), "pg");
/// assert_eq!(doc.to_lin(), movie);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinDocument {
    /// The complete `command|value|` pairs, in order.
    pub tokens: Vec<LinToken>,
    /// Whatever follows the last complete pair: usually a line ending or
    /// nothing, or a command whose value was never closed.
    pub trailing: String,
}

impl LinDocument {
    /// Split a LIN text into its tokens. Never fails: text that does not form
    /// a complete pair is kept in [`Self::trailing`].
    pub fn parse(text: &str) -> Self {
        let pieces: Vec<&str> = text.split('|').collect();
        // Each complete pair uses two pipes; `pieces.len() - 1` is the count.
        let complete = (pieces.len() - 1) / 2;
        let tokens = pieces
            .chunks(2)
            .take(complete)
            .map(|pair| LinToken {
                raw_command: pair[0].to_string(),
                value: pair[1].to_string(),
            })
            .collect();
        let trailing = pieces[complete * 2..].join("|");
        Self { tokens, trailing }
    }

    /// The text, exactly as parsed plus any edits to the tokens.
    pub fn to_lin(&self) -> String {
        let mut out = String::new();
        for token in &self.tokens {
            out.push_str(&token.raw_command);
            out.push('|');
            out.push_str(&token.value);
            out.push('|');
        }
        out.push_str(&self.trailing);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_and_what_follows_them() {
        let doc = LinDocument::parse("pn|a,b,c,d|md|3SAK|\r\n");
        assert_eq!(
            doc.tokens,
            [LinToken::new("pn", "a,b,c,d"), LinToken::new("md", "3SAK")]
        );
        assert_eq!(doc.trailing, "\r\n");
    }

    #[test]
    fn an_unclosed_value_is_kept() {
        let doc = LinDocument::parse("pg||at|no closing pipe");
        assert_eq!(doc.tokens, [LinToken::new("pg", "")]);
        assert_eq!(doc.trailing, "at|no closing pipe");
        assert_eq!(doc.to_lin(), "pg||at|no closing pipe");
    }

    #[test]
    fn empty_text() {
        let doc = LinDocument::parse("");
        assert!(doc.tokens.is_empty());
        assert_eq!(doc.to_lin(), "");
    }

    #[test]
    fn whitespace_around_a_command_is_kept_but_not_part_of_its_name() {
        let doc = LinDocument::parse("at|text|\n\npg||");
        assert_eq!(doc.tokens[1].raw_command, "\n\npg");
        assert_eq!(doc.tokens[1].command(), "pg");
        assert!(doc.tokens[1].is("PG"));
    }

    #[test]
    fn every_fixture_writes_back_byte_for_byte() {
        let fixtures = [
            include_str!("../../fixtures/lin/movie-constructs.lin"),
            include_str!("../../fixtures/lin/alerts-notes-play-bc.lin"),
            include_str!("../../fixtures/lin/names-with-delimiters-bc.lin"),
            include_str!("../../fixtures/lin/continue-marker-bc.lin"),
            include_str!("../../fixtures/lin/nag-and-notrump-bc.lin"),
            include_str!("../../fixtures/lin/voids-bc.lin"),
            include_str!("../../fixtures/lin/no-auction-bc.lin"),
            include_str!("../../fixtures/lin/date-only-bc.lin"),
            include_str!("../../fixtures/lin/two-boards-bc.lin"),
            include_str!("../../fixtures/lin/bc-reexport-of-read-four-hands.lin"),
            include_str!("../../fixtures/lin/read-plus-alerts-claim.lin"),
            include_str!("../../fixtures/lin/read-four-hands.lin"),
            include_str!("../../fixtures/lin/read-one-hand.lin"),
            include_str!("../../fixtures/lin/read-two-hands.lin"),
            include_str!("../../fixtures/lin/read-percent-encoding.lin"),
            include_str!("../../fixtures/lin/read-alert-without-text.lin"),
            include_str!("../../fixtures/lin/read-one-board-per-line.lin"),
        ];
        for text in fixtures {
            assert_eq!(LinDocument::parse(text).to_lin(), text);
        }
    }

    #[test]
    fn a_movie_keeps_every_construct() {
        let doc = LinDocument::parse(include_str!("../../fixtures/lin/movie-constructs.lin"));
        let commands: Vec<&str> = doc.tokens.iter().map(LinToken::command).collect();
        for expected in [
            "st", "nt", "sk", "at", "pg", "hs", "ls", "hc", "lc", "up", "tc", "At", "Hs", "HC",
        ] {
            assert!(commands.contains(&expected), "{expected} kept");
        }
        let plays: Vec<&str> = doc
            .tokens
            .iter()
            .filter(|t| t.is("pc"))
            .map(|t| t.value.as_str())
            .collect();
        for movie_play in ["h", "!da", "!sa", "s2hdc", ""] {
            assert!(plays.contains(&movie_play), "pc|{movie_play}| kept");
        }
        assert!(doc
            .tokens
            .iter()
            .any(|t| t.is("md") && t.value.starts_with('0')));
    }
}
