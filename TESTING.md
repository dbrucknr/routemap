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

---

## Fuzz testing

Located in `fuzz/fuzz_targets/lpm_ops.rs`. The target generates arbitrary sequences
of table operations — `insert`, `remove`, `longest_match`, `longest_match_entry`,
`get`, `contains`, `clear` — and drives them into a live `RouteMap`. Any panic,
out-of-bounds access, or undefined behaviour is reported as a crash.

This is the strongest test layer for the bit-manipulation core: the `match_mask` /
`leading_zeros` arithmetic in `longest_match_impl` and the `rank`-indexed `Vec`
accesses are exactly the kind of code that fuzzing finds bugs in that proptest
shrinking misses.

### Prerequisites

`cargo-fuzz` requires a nightly Rust toolchain:

```sh
rustup toolchain install nightly
cargo +nightly install cargo-fuzz --locked
```

### Running the fuzzer

From the repository root:

```sh
# Run indefinitely — Ctrl-C to stop
cargo +nightly fuzz run lpm_ops

# Save coverage-increasing inputs to a local corpus dir
mkdir -p fuzz/corpus/lpm_ops
cargo +nightly fuzz run lpm_ops fuzz/corpus/lpm_ops

# Run for a fixed duration (seconds)
cargo +nightly fuzz run lpm_ops fuzz/corpus/lpm_ops -- -max_total_time=300

# List all targets
cargo +nightly fuzz list
```

**macOS (Apple Silicon):** libFuzzer's AddressSanitizer is not supported on
`aarch64-apple-darwin`. Add `-s none` to disable it:

```sh
cargo +nightly fuzz run lpm_ops -s none -- -max_total_time=300
```

The fuzzer prints a status line every few seconds:

```
#1234   NEW    cov: 87 ft: 102 corp: 5/320b exec/s: 12345 rss: 64Mb
```

| column | meaning |
|---|---|
| `cov` | source-code edges covered so far |
| `corp` | number of interesting inputs saved to the corpus |
| `exec/s` | fuzzer throughput |
| `rss` | current memory usage |

The corpus in `fuzz/corpus/lpm_ops/` is reused on the next run, so coverage
accumulates across sessions. Corpus files and crash artifacts are gitignored.

### Investigating a crash

When the fuzzer finds a crash it writes the reproducer to
`fuzz/artifacts/lpm_ops/crash-<hash>`. Reproduce it deterministically with:

```sh
cargo +nightly fuzz run lpm_ops fuzz/artifacts/lpm_ops/crash-<hash>
```

Minimise the input to the smallest case that still triggers the crash:

```sh
cargo +nightly fuzz tmin lpm_ops fuzz/artifacts/lpm_ops/crash-<hash>
```

Then add a regression test to `mod tests` or `mod prop_tests` so the case is covered
forever.

### CI integration

For CI, a short smoke run catches obvious regressions against whatever corpus has
been seeded:

```sh
cargo +nightly fuzz run lpm_ops fuzz/corpus/lpm_ops -s none -- -max_total_time=30
```

30 seconds is not enough to explore deep paths — for sustained coverage, run locally
overnight or on a dedicated machine.
