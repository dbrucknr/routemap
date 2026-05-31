# Testing

routemap uses two layers of tests: **unit tests** that verify specific, hand-crafted
scenarios and **property tests** that verify invariants across randomly generated inputs.

---

## Unit tests

Located in `src/treebitmap/mod.rs` alongside the implementation. They cover named
scenarios that are easy to reason about: the default route, /32 host routes,
non-stride-aligned prefix lengths (/1, /2, /3, /10), remove-then-fallback, and edge
cases like an empty table or overwriting an existing prefix.

Run them with:

```
cargo test
```

---

## Property tests

Located in the `prop_tests` module at the bottom of `src/treebitmap/mod.rs`. Rather
than picking specific inputs, property tests describe **invariants that must hold for
all valid inputs** and let [proptest](https://github.com/proptest-rs/proptest) generate
hundreds of random cases automatically.

This is especially valuable for a treebitmap because the correctness of each operation
depends on precise bitmap arithmetic (the `rank`/popcount trick, internal/external
bitmap updates, and compact Vec indexing). A single off-by-one in any of those can
produce wrong answers only for certain prefix lengths or address patterns — exactly
the kind of bug that hand-written tests miss and random inputs find.

### Invariants

#### Insert → retrieval roundtrip
**Property:** After inserting any prefix `P` with value `V`:
- `contains(P)` returns `true`
- `get(P)` returns `Some(&V)`
- `longest_match(network_address(P))` returns `Some(&V)`

*Why:* Verifies that the internal and external bitmaps are set correctly on insert and
that rank-based indexing maps back to the right slot in the compact value Vec.

---

#### Masked equivalence
**Property:** Inserting `addr/len` (with host bits set) produces the same table state
as inserting `(addr & mask(len))/len`.

*Why:* The implementation silently masks the prefix on insert. This property confirms
that host bits never contaminate the stored network address or affect lookups.

---

#### Overwrite semantics
**Property:** Inserting the same prefix twice leaves `len() == 1` and `get()` returns
the second value.

*Why:* Catches any double-counting in the entry counter or failure to replace the
existing value in the compact Vec.

---

#### Remove consistency
**Property:** After inserting then removing a prefix `P`:
- `remove(P)` returns the original value
- `contains(P)` returns `false`
- `get(P)` returns `None`
- `len()` returns `0`

*Why:* Removal has to clear the internal bitmap bit, remove the value from the compact
Vec at the right rank-computed index, and optionally prune empty child nodes. Any
mistake in that sequence leaves the table in an inconsistent state.

---

#### `len` invariant
**Property:** After any sequence of inserts, `len()` equals `iter().count()`.

*Why:* `len` is maintained as a separate counter. This property confirms it stays in
sync with the actual number of entries regardless of overwrites.

---

#### LPM correctness
**Property:** When both a broad prefix `/B` and a more-specific prefix `/S` (where
`S > B`) cover a lookup address, `longest_match` returns the value from `/S`.

*Why:* This is the core semantic guarantee of a longest-prefix-match table. The
treebitmap scans internal bitmap positions from least to most specific within each
stride, so the last hit in the scan wins. A bug in that ordering would return the
wrong prefix here.

---

#### Fallback after remove
**Property:** After removing the more-specific prefix `/S`, `longest_match` on the
same address returns the value from the covering prefix `/B`.

*Why:* Combines remove correctness with LPM correctness. Ensures that pruning an
empty child node does not accidentally orphan the value stored in a parent node.

---

#### Default route universality
**Property:** A `/0` default route matches every possible address.

*Why:* `/0` is stored at the root node's catch-all bitmap position (`bpos = 1`,
`rel_len = 0`). This is the degenerate case where the prefix consumes zero bits of
the address. If the lookup loop mishandles it, no address would ever match.

---

#### Host route exclusivity
**Property:** A `/32` (IPv4) host route matches its own address and no other.

*Why:* `/32` is stored at maximum depth — the node reached after all 8 strides. The
implementation has a special post-loop check for this case. If that check is missing
or fires incorrectly, host routes either never match or match too broadly.

---

#### Iterator completeness
**Property:** Collecting the output of `iter()` into a new `RouteMap` via `collect()`
produces a table with the same `len()` and the same value for every prefix.

*Why:* The iterator reconstructs each prefix from accumulated nibble bits and the
internal bitmap position. A decoding error would produce a wrong prefix string, making
the round-tripped table diverge from the original.

---

#### IPv6 parity
The three most fundamental properties — insert/retrieval roundtrip, default route
universality, and LPM correctness — are repeated for IPv6 with randomly generated
128-bit addresses and prefix lengths 0–128.

*Why:* The IPv4 and IPv6 paths share the same generic implementation, but IPv6 uses
32 strides instead of 8. Bugs in stride boundary arithmetic are much harder to trigger
with 32-bit inputs alone.
