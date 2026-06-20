# routemap — Implementation Plan

This document is both a roadmap and a learning guide. Each phase builds on the last.
Take as much time as needed between steps — understanding *why* each piece exists
matters more than moving fast.

---

## Phase 1 — Foundation

Before writing any trie code, we need to understand the building blocks we'll use
and set up the crate so it is ready to publish when the time comes.

### 1.1 Crate metadata
- [x] Fill in `Cargo.toml` — `description`, `license`, `repository`, `keywords`, `categories`
- [x] Add `readme = "README.md"` so crates.io renders our documentation

**Why this matters:** crates.io uses these fields for search and discovery.
A crate with no description or keywords is effectively invisible.

---

### 1.2 Understand `IpPrefix<A>` from ipnetx
- [x] Read through the ipnetx source for `IpPrefix<A>` and the `IpAddress` trait
- [x] Understand `addr.to_u128()` — this is how we extract individual bits during trie traversal
- [x] Understand `prefix.mask()` — this tells us how deep in the trie a prefix lives

**Why this matters:** The trie does not store prefixes as strings or structs — it
encodes them as paths through the tree. Every bit of the address is a left (0) or
right (1) turn. `to_u128()` is the bridge between an IP prefix and a sequence of
bit decisions.

**The bit extraction formula:**
```
bit N of address = (addr.to_u128() >> (W - 1 - N)) & 1
```
where W = 32 for IPv4, 128 for IPv6, and N counts from 0 at the most-significant bit.

---

## Phase 2 — Core Data Structures

This is the most conceptually important phase. We define what a node in the trie
looks like and how nodes relate to each other.

### 2.1 Define `TrieNode<V>`
- [x] Create `src/node.rs`
- [x] Define a node with: two optional child pointers (`children: [Option<Box<TrieNode<V>>>; 2]`)
      and an optional stored value (`value: Option<V>`)
- [x] **Arena refactor:** replaced `Box`-based nodes with a flat `Vec<ArenaNode<V>>`
      and `u32` indices (NULL sentinel = `u32::MAX`), eliminating per-node heap
      allocations and improving cache locality

---

## Phase 3 — Core Algorithm

With the node structure in place, we implement the two operations the entire crate
is built around. Everything else is API surface on top of these.

### 3.1 `insert(prefix: IpPrefix<A>, value: V)`
- [x] Create `src/table.rs` with the `RouteMap<V>` struct (owns the root node)
- [x] Implement `insert`: walk the trie bit by bit for `prefix_len` steps, creating
      child nodes as needed, then store `value` at the final node

---

### 3.2 `longest_match(addr: A) -> Option<&V>`
- [x] Implement `longest_match`: walk the trie bit by bit for all W bits, and at
      every node that has a stored value, update a "best match so far" register
- [x] After the walk, return whatever is in the register

---

## Phase 4 — API Completeness

The trie works. Now we round out the public API to make it genuinely useful.

### 4.1 `remove(prefix: &IpPrefix<A>) -> Option<V>`
- [x] Walk to the node at the prefix's depth, take the value out, return it
- [x] Prune any now-childless, now-valueless nodes on the way back up

---

### 4.2 `contains(prefix: &IpPrefix<A>) -> bool`
- [x] Walk to the node at the prefix's depth, return whether it holds a value

---

### 4.3 `iter()` — iterate all `(IpPrefix<A>, &V)` pairs

> **Deferred to Phase 7.** Building `iter()` now and rebuilding it after the
> treebitmap rewrite (Phase 6) is pure churn. Implement it once on the final layout.

---

## Phase 5 — Benchmarks (Baseline)

### 5.1 Criterion setup
- [x] Add `criterion = "0.8"` as a dev-dependency
- [x] Create `benchmarks/lookup.rs` with benchmarks for `insert` and `longest_match`
      at 1 000 / 10 000 / 100 000 prefixes for both IPv4 and IPv6
- [x] Use realistic, seeded prefix length distributions (BGP-shaped: IPv4 favors
      /16–/28, IPv6 favors /48–/64); lookup addresses use a separate seed for a
      realistic hit/miss mix
- [x] Record baseline numbers in `BENCHMARKS.md`

---

## Phase 6 — Treebitmap

### 6.1 Understand the algorithm
- [x] Worked through the algorithm in detail: internal bitmap (15 positions, binary
      heap indexed 1–15), external bitmap (16 bits), popcount-rank trick
- [x] Traced a concrete 3-prefix example (0.0.0.0/0, 10.0.0.0/8, 10.20.0.0/16)
      through insert and lookup by hand
- [x] Saved full algorithm reference to `treebitmap.md`

### 6.2 Choose a stride
- [x] Stride 4 selected — 4-bit nibbles, 15-bit internal bitmap, 16-bit external
      bitmap; fits well in cache lines, bitmap math stays manageable

### 6.3 Implementation
- [x] Archived binary trie to `src/arena/` (all 31 tests retained and passing)
- [x] Defined `TbNode<V>` with `internal: u32`, `external: u32`, `values: Vec<V>`,
      `children: Vec<TbNode<V>>`
- [x] Implemented `rank` (popcount) function with 5 isolated unit tests
- [x] Implemented `insert` — stride-hop navigation, internal bitmap set, value
      inserted at rank-computed index; handles all `rel_len` 0–3 cases
- [x] Implemented `longest_match` — checks 4 internal positions per node, follows
      external bitmap; fixed off-by-one for max-depth prefixes (`/32`, `/128`)
- [x] Implemented `remove` — clears internal bit, removes value at rank index,
      prunes empty nodes on the way back up
- [x] Implemented `contains`
- [x] 100% line, region, and function coverage (`cargo llvm-cov`)
- [x] Ran Phase 5 benchmarks against treebitmap; filled in `BENCHMARKS.md`

**Results at 100k prefixes:**

| | IPv4 | IPv6 |
|---|---|---|
| Lookup throughput | 25 M/s (1.35× faster) | 47 M/s (3.54× faster) |
| Insert throughput | 14 M/s (≈ same) | 3.5 M/s (1.1× slower) |

---

## Phase 7 — Quality and Polish

### 7.1 `iter()` — iterate all `(IpPrefix<A>, &V)` pairs
- [x] Implement a stack-based DFS iterator that reconstructs each prefix from the
      accumulated nibble path and internal bitmap position (rel_len + rel_bits →
      full_len + full_addr via `A::from_u128` and `IpPrefix::new`)
- [x] 9 targeted tests covering empty table, /0, /32, non-stride-aligned lengths,
      IPv6, and post-remove correctness

**Why this matters:** Callers need to inspect the full table — for serialization,
debugging, or feeding results into an `ipnetx` `IpSetBuilder`. With the treebitmap
layout, traversal must decode internal bitmap positions back into prefix lengths.

---

### 7.2 Standard trait implementations
- [x] `impl Default for RouteMap<A, V>`
- [x] `impl Debug for RouteMap<A, V> where A: Display, V: Debug` — renders as `{"10.0.0.0/8": value, ...}`
- [x] `impl FromIterator<(IpPrefix<A>, V)> for RouteMap<A, V>` — allows `collect()` into a table
- [x] ArenaTable removed from public API — unexported in v0.1 (no iter(), no doc comments,
      deferred to v0.2 with feature parity)

---

### 7.3 Documentation
- [x] Doc comments on all public types and methods, each with at least one `# Example`
      doctest (verified passing with `cargo test`)
- [x] `# Performance` section on `RouteMap` struct documenting the insert/lookup
      trade-off with real benchmark numbers
- [x] README rewritten: Quick Start, full API reference, real benchmarks, treebitmap
      explainer, "Why Not a HashMap?", "Common Mistakes"
- [x] Add `#[doc = include_str!("../README.md")]` to `lib.rs` so docs.rs renders the README

---

### 7.4 Testing improvements

The current suite has 148 passing tests (unit + proptest) and previously achieved
100% line coverage. The gaps below are about correctness confidence, not coverage numbers.

**High priority**

- [x] **Fuzz target (`cargo-fuzz`).** Add a libfuzzer target that feeds random
      `(addr: u32/u128, len: u8)` sequences to `insert`, `remove`, and
      `longest_match`. The `match_mask` / `leading_zeros` bit arithmetic in
      `longest_match_impl` is exactly what fuzzing catches that proptest shrinking misses.
- [x] **`remove` isolation property test.** `remove_clears_entry` only tests
      single-entry tables. Add a property test that inserts N random prefixes, removes
      one, and asserts every other prefix is still accessible via `get` / `contains` /
      `longest_match`.
- [x] **`nibble()` direct unit tests.** The function is called on every hop but has
      no isolated tests. Cover all 16 nibble values at each stride position for both
      IPv4 (positions 0–7) and IPv6 (positions 0–31), verifying the
      `addr_bits - (hop + 1) * STRIDE` shift at its boundaries.

**Medium priority**

- [x] **`Clone` independence.** `RouteMap` derives `Clone` but no test mutates a
      clone and verifies the original is unaffected (and vice versa).
- [x] **IPv6 `remove` property test.** IPv6 prop tests cover insert/contains/get/LPM
      but not remove. Mirror `remove_clears_entry` and `remove_specific_falls_back_to_broad`
      for the 128-bit address space.
- [x] **`clear()` at scale.** The existing `clear_then_reinsert_works` test only
      clears 1 entry. Add a property test: insert N random prefixes → `clear` →
      assert `len == 0` and `is_empty` → re-insert the same prefixes → verify
      all lookups return the expected values.
- [x] **Iteration order assertion.** The docs promise depth-first order (shorter
      prefixes at a node before its children). Add a deterministic test that builds a
      known tree and asserts the exact sequence yielded by `iter`.

**Lower priority**

- [x] **`rank()` edge cases.** Add unit tests for position 0, position 31, and all
      bits set (`u32::MAX`).
- [ ] **Coverage gate in CI.** Enforce a minimum `llvm-cov` line coverage floor so
      regressions are caught automatically.

---

### 7.5 `serde` support
- [ ] Add `serde` as an optional dependency behind a `serde` feature flag
- [ ] Implement `Serialize` / `Deserialize` for `RouteMap<A, V> where V: Serialize + DeserializeOwned`
- [ ] Add a feature-gated doc example showing round-trip through `serde_json`

**What to watch for:** Serialize as a flat list of `{ prefix, value }` records and
rebuild via `FromIterator` on deserialization. Do not serialize the internal trie
structure — that would couple the wire format to the implementation.

---

### 7.5 Publish
- [ ] `cargo test` — all tests pass
- [ ] `cargo doc --open` — documentation looks correct
- [ ] `cargo publish --dry-run` — no packaging errors
- [ ] `cargo publish`
- [ ] Update the `ipnetx` README to cross-reference `routemap`

---

## Phase Order Summary

```
1.1 Cargo metadata                              ✓
1.2 Understand IpPrefix<A> and bit extraction   ✓
    ↓
2.1 Define TrieNode<V> → ArenaNode<V>           ✓
    ↓
3.1 Implement insert                            ✓
3.2 Implement longest_match                     ✓
    ↓
4.1 Implement remove                            ✓
4.2 Implement contains                          ✓
4.3 Implement iter                              → deferred to 7.1
    ↓
5.1 Criterion benchmarks (binary trie baseline) ✓
    ↓
6.1 Understand treebitmap algorithm             ✓
6.2 Choose stride (4)                           ✓
6.3 Implement treebitmap + benchmark comparison ✓
    ↓
7.1 iter()                                      ✓
7.2 Trait impls (Default, Debug, FromIterator)  ✓
7.3 Documentation                               ✓
7.4 Testing improvements
7.5 serde support
7.6 Publish
```

Should this project define traits that allow the functionality to be extended?
Should this project also interoperate with netip?
- or maybe netipx needs to support those data structures?

Add:

CI badges
benchmarks
fuzz testing
MSRV policy
changelog
examples directory

Is any if this tokio supportable?
- It all looks like CPU intensive work to me, not I/O bound.

Answer in README.md
Why this crate exists?
When to use it?

Change name of directory from iplookup to routemap
