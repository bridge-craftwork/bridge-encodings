# The Bridge Composer LIN oracle

What Bridge Composer 5.118.2 does when it reads and writes LIN, captured headless
on Windows (`BridgeComposer.Object` with `Noui = true`) for issue #31. Every deal
here was written for the probe; none comes from a published hand set.

The `-bc` files are Bridge Composer's output and are CRLF, which is what it
writes; `.gitattributes` marks them `-text` so git leaves them alone.

## Writing: `<name>.pbn` → `<name>-bc.lin`

`ExportAsBridgeBaseOnline` on the PBN.

| Input | What it pins down |
|-------|-------------------|
| `alerts-notes-play.pbn` | Names with spaces; `!`, `=n=` and `$2` on calls; play past trick one with a different leader; `Result` with the play incomplete |
| `names-with-delimiters.pbn` | A comma and a pipe in player names and a note — Bridge Composer writes them raw and breaks the line |
| `continue-marker.pbn` | A `+` placeholder in the auction |
| `nag-and-notrump.pbn` | `$1` on a call, and a notrump grand slam |
| `voids.pbn` | Four voids per hand |
| `no-auction.pbn` | No auction, contract or result |
| `date-only.pbn` | `Date` without `Event` |
| `two-boards.pbn` | Two boards in one file: a tournament header, then each board as it would be alone |

`bc-reexport-of-read-four-hands.lin` is `read-four-hands.lin` opened and exported
again.

**The layout**, per board:

```
mn|<Event> - <Date>|pn|S,W,N,E|qx|o<n>,BOARD <n>|rh||ah|Board <n>|md|<dealer>S,W,N|sv|<0|n|e|b>|
sa|0|mb|...|an|...|pg||
pc|..|pc|..|pc|..|pc|..|pg||        one line per trick, in play order
mc|<Result>|pg||
```

- `mn` is `Event - Date`, `Event` alone when there is no date, and ` - Date`
  when there is no event.
- Hands in `md` are South, West, North; each writes all four suit letters, so a
  void is two letters together.
- Calls are `1N` … `7N`, `p`, `d`, `r`. A call is never written `mb|1C!|`:
  `!` and `$1` become `an|!|`, `$2` becomes `an|?|`, and `=n=` becomes the note's
  text in `an`. A `+` placeholder is dropped.
- `mc` follows `Result`. A board with a contract and no result gets `mc|0|`,
  which claims no tricks; a board with neither gets no `mc` at all.
- Nothing is escaped: a comma or pipe in a name or note is written as is.
- A multi-board file opens with `vg`, `rs`, `pw`, `mp`, `bn` and `pg` records.
  In that file a board with no auction was given three passes.

**Boards it will not export.** `bc-cannot-export.pbn` holds one board for each
thing that stops the export behind a dialog: a board id that is not a number
(`1-1`), hidden hands, a `=n=` with no matching note, two note references on
one call, and notes not numbered from 1. There is no `-bc` output for these.

## Reading: `read-<name>.lin` → `read-<name>-bc.pbn`

`Open` on the LIN, then `SaveAs` PBN.

| Input | What it pins down |
|-------|-------------------|
| `read-plus-alerts-claim.lin` | `+` in `pn`, `ah` and `an`; alerts with text; three hands in `md`; `mc` |
| `read-four-hands.lin` | Four hands in `md`, `1NT`, capital `P`/`D`/`R`, `sv|0|` |
| `read-one-hand.lin` | One hand in `md` |
| `read-two-hands.lin` | South and North only |
| `read-percent-encoding.lin` | `%20` and `%2B` |
| `read-alert-without-text.lin` | `mb|1n!|` with no `an`, lower-case calls |
| `read-one-board-per-line.lin` | Two boards, one per line |

**What it shows:**

- No decoding: `+` and `%xx` are kept as written in `pn`, `ah` and `an`. So
  `ah|Board+7|` does not give board 7.
- An alert becomes the `$1` annotation; an `an` becomes a `=n=` reference and a
  `[Note]`, whether or not the call was alerted.
- The fourth hand is computed only when the other three are complete. Otherwise
  the missing hands stay unknown.
- `mc|n|` becomes `[Result "n"]`.
