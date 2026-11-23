# The `DoubleDummyPlay` section

A PBN extension carrying card-by-card double-dummy analysis of a board's play:
for every card played, what each card that could have been played instead
would have cost. It is not in PBN 2.1. This document is its specification, and
`src/pbn/dd_play.rs` is its reference reader and writer.

It sits beside `[DoubleDummyTricks]` (a Bridge Composer extension) and
`[OptimumResultTable]` (PBN 2.1 §5.7), which give the double-dummy result of
the deal before any card is played. This section follows the play from there.

## Example

```
[Deal "N:QJ7.8.T9752.J752 K542.T7.QJ6.Q943 A863.Q932.K84.K8 T9.AKJ654.A3.AT6"]
[Declarer "S"]
[Contract "3D"]
[Play "W"]
HA H8 H7 H2
ST SJ S5 S3
...
[DoubleDummyPlay "W"]
11.000000.00.111 .0.. .00.. .0001..
00.00000.00.000 000... 0000... 0010...
...
```

(The digits here show the shape; real values come from a solver.)

## Layout

The section mirrors the `[Play]` section it analyses (PBN 2.1 §3.6):

- **The tag value is the opening leader**, as in `[Play]`, and must match it.
- **The data is a stream of entries** separated by whitespace. Entry `k`
  belongs to the card token `k` of `[Play]`: the same trick (`k / 4`) and the
  same column (`k % 4`), where columns run clockwise from the *opening*
  leader, not from each trick's leader. A writer puts each trick on its own
  line, four entries to a line, exactly as `[Play]` is laid out.
- **`*` or `+` may close the section**, as in `[Play]`.
- **`-` is an entry not analysed.** It must be used where `[Play]` has `-`
  (a card not on record), and may be used anywhere else.

## An entry

An entry is the hand the player held at that turn, written as in `[Deal]`:
four suit groups, spades, hearts, diamonds, clubs, separated by `.`. In place
of each card is a single character for that card:

- `0`–`9`, `a`–`d`: the card's **cost**, 0 to 13. See below.
- `?`: the card was legal but not solved.

The cards themselves are not named. A reader rebuilds the hand from `[Deal]`
less the cards that player played to earlier tricks in `[Play]`, sorts each
suit high to low, and pairs the characters with the cards in that order.

**Only legal cards are written.** Legality always goes by whole suits: a player
who can follow suit may play only that suit, and one who leads or cannot
follow may play any card. A group is therefore either

- empty, when the player holds no card of the suit or may not play it, or
- exactly as long as the player's holding in it.

Any other length is an error. So is a played card (from `[Play]`) that is not
among the legal cards, or a group for a suit that is not legal.

A player following to a heart lead with ♥QJ3 and a spade void writes `.001..`:
spades void, hearts three legal cards, diamonds and clubs not legal.

## The cost

A card's cost is how many tricks it gives away, double dummy, to the side
whose turn it is, against that side's best choice at the same turn:

- every card that keeps the side's best result costs `0`;
- one that gives one trick away costs `1`, and so on.

So every complete entry has at least one `0`, and the cost of the card
actually played is the character in its place. Costs are the same for both
sides: a defender's `1` is a trick handed to declarer, a declarer's `1` a
trick handed to the defence. A display that signs them against declarer
(−1 for declarer's error, +1 for a defender's) does so from the seat.

The double-dummy result before the opening lead is not repeated here; it is
the declarer's cell of `[DoubleDummyTricks]` or `[OptimumResultTable]`.

## What a reader can derive

- **Each card's cost**, and the running double-dummy result through the play.
- **Wrong card or wrong suit.** When a played card costs something, it was
  the wrong card in a good suit if another card in its suit costs `0`, and the
  wrong suit if none does. Displays colour the two differently.
- **The alternatives at any turn**, for a view that opens a trick and shows
  what each card would have done.

## Reading rules

- A section that fails any rule above is dropped whole, as a malformed
  `DoubleDummyTricks` is: half an analysis silently shown as a whole one is
  worse than none.
- The section may stand before or after `[Play]` in the record; it is decoded
  once the record has been read.
- It needs `[Deal]` and `[Play]`. Without them, or without a lead on record
  for a trick, it cannot be decoded and is dropped.
- Readers that do not know the section skip its data lines, as PBN 2.1 asks
  of unknown sections. (bridge-encodings readers before this one dropped the
  lines and kept the tag, which a writer then re-emitted empty.)
