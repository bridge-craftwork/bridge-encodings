# The Bridge Composer placement oracle

Two files, and a human round trip that cannot be repeated by CI.

| File | What it is |
|------|------------|
| `pbn-order-test.pbn` | The input. Eight boards, hand-written, with the 15 mandatory tags deliberately shuffled in every board and the supplemental tags put somewhere different in each. |
| `pbn-order-test-bc.pbn` | The same file after Bridge Composer 5.118.2 opened it, took a trivial edit, and saved. |

Both are CRLF, which is what Bridge Composer writes; `.gitattributes` marks them
`-text` so git leaves them alone. A diff against them is only meaningful byte
for byte.

**The control.** Every board comes back with its 15 mandatory tags restored to
the standard's fixed order, however they were shuffled going in. That proves
Bridge Composer normalises rather than copying the input through, which is what
makes the rest of its output evidence.

**The layout it produces**, in every board:

1. the 15 mandatory tags, in the standard's order (PBN 2.1 §3.4);
2. supplemental **tag pairs**, sorted alphabetically, including names Bridge
   Composer has never seen — board 7's `AAACustom` sorts above `BCFlags`,
   `ZZZCustom` below `ParContract` (§3.4);
3. `[Auction]` and its calls (§3.1);
4. `[Play]` and its cards (§3.1);
5. supplemental **sections**, sorted alphabetically among themselves — board 8's
   custom `AAATable` comes out before `OptimumResultTable`, and both after the
   auction even though `AAATable` was written above it (§3.1).

So `DoubleDummyTricks`, `OptimumScore` and `ParContract` are group 2 while
`OptimumResultTable` is group 5: the one-line summaries go up among the
identification tags, the twenty-row table goes to the bottom. That is the rule
`tag_rank` in `src/pbn/document.rs` implements, and
`bridge_composer_output_is_already_in_rank_order` and
`reinserting_every_supplemental_tag_reproduces_bridge_composers_order` check it
against these files.

These rules are the standard's **export** format. §3.4 says plainly that "for
import format, the order of tag pairs is not important", so nothing here
justifies rejecting or reordering a document on read. It governs only where a
newly inserted tag lands.

The originals live in `bridge-solver` under `fixtures/bridge-composer/`,
alongside a third file carrying Bridge Composer's own double-dummy analysis and
a longer account of what else they prove.
