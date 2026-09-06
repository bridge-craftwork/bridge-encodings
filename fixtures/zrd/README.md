# ZRD fixtures

`rpdd_10First.zrd` — the first ten records of Richard Pavlicek's solved-deal
library, as shipped with DealerV2_4. 230 bytes, ten 23-byte records, no
separators.

This is a **golden fixture**: the expected deals and tables are written out as
static data in `src/zrd/mod.rs`'s tests. Those expectations were established
once by re-solving each record's deal with DDS and comparing all twenty cells;
all ten records agreed exactly. Nothing re-solves at test time, and this crate
takes no solver dependency — the verification lives in the committed numbers.

Round-trip tests alone could not do this job: a transposed strain or seat axis
round-trips perfectly, because packing and unpacking are inverses whichever
order they agree on. Only expected values sourced from outside this crate pin
the axes down.
