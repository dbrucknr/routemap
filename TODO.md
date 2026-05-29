# iplookup — Implementation Plan

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
- [x] Create `src/table.rs` with the `IpTable<V>` struct (owns the root node)
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
- [ ] Implement an in-order traversal of the treebitmap that reconstructs each prefix
      from the path and internal bitmap position taken to reach its node

**Why this matters:** Callers need to inspect the full table — for serialization,
debugging, or feeding results into an `ipnetx` `IpSetBuilder`. With the treebitmap
layout, traversal must decode internal bitmap positions back into prefix lengths.

---

### 7.2 Standard trait implementations
- [ ] `impl Default for IpTable<A, V>`
- [ ] `impl Debug for IpTable<A, V> where V: Debug`
- [ ] `impl FromIterator<(IpPrefix<A>, V)> for IpTable<A, V>` — allows `collect()` into a table

---

### 7.3 Documentation
- [x] Doc comments on all public types and methods, each with at least one `# Example`
      doctest (verified passing with `cargo test`)
- [x] `# Performance` section on `IpTable` struct documenting the insert/lookup
      trade-off with real benchmark numbers
- [x] README rewritten: Quick Start, full API reference, real benchmarks, treebitmap
      explainer, "Why Not a HashMap?", "Common Mistakes"
- [x] Add `#[doc = include_str!("../README.md")]` to `lib.rs` so docs.rs renders the README

---

### 7.4 `serde` support
- [ ] Add `serde` as an optional dependency behind a `serde` feature flag
- [ ] Implement `Serialize` / `Deserialize` for `IpTable<A, V> where V: Serialize + DeserializeOwned`
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
- [ ] Update the `ipnetx` README to cross-reference `iplookup`

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
7.1 iter()                                      ← next
7.2 Trait impls (Default, Debug, FromIterator)
7.3 Documentation                               ✓
7.4 serde support
7.5 Publish
```
