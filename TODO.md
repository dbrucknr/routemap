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

**Why this matters:** Each node in the trie represents one bit position along a path.
The `children[0]` branch is taken when the current bit is `0`; `children[1]` when it
is `1`. A node that has a `value` is a prefix endpoint — it is where we record a
match during lookup. A node without a value is just a structural intermediary that
exists to connect two prefixes that share a common bit prefix.

**Example tree with two prefixes inserted:**
```
root
 └─[0]─ ...
         └─[0]─ ...
                 └─[0]─ ... (depth 8: "10.0.0.0/8" stored here, value = RouteA)
                               └─[0]─ ...
                                       └─[0]─ ... (depth 16: "10.20.0.0/16" stored here, value = RouteB)
```

---

## Phase 3 — Core Algorithm

With the node structure in place, we implement the two operations the entire crate
is built around. Everything else is API surface on top of these.

### 3.1 `insert(prefix: IpPrefix<A>, value: V)`
- [x] Create `src/table.rs` with the `IpTable<V>` struct (owns the root node)
- [x] Implement `insert`: walk the trie bit by bit for `prefix_len` steps, creating
      child nodes as needed, then store `value` at the final node

**Why this matters:** Insert teaches you the trie's write path. You are essentially
converting an IP prefix into a series of left/right decisions and placing the value
at the exact depth that corresponds to the prefix length. A `/8` prefix lives at
depth 8; a `/24` prefix lives at depth 24.

**What to watch for:** You only walk `prefix_len` bits — not all 32 or 128. The
remaining bits do not matter for this prefix, so you stop early.

---

### 3.2 `longest_match(addr: A) -> Option<(IpPrefix<A>, &V)>`
- [x] Implement `longest_match`: walk the trie bit by bit for all W bits, and at
      every node that has a stored value, update a "best match so far" register
- [x] After the walk, return whatever is in the register

**Why this matters:** This is the payoff. Lookup is simpler than insert — you never
create nodes, you only follow them. The key insight is that you do *not* stop at the
first match; you keep walking and overwrite the register with any deeper match you
find. When the walk ends, the register holds the most specific (longest) match.

**What to watch for:** The walk follows the bits of the *full address* — all W bits —
not just the prefix length of anything stored. You are trying to go as deep as
possible; the tree's structure will terminate the walk when no child exists for the
next bit.

---

## Phase 4 — API Completeness

The trie works. Now we round out the public API to make it genuinely useful.

### 4.1 `remove(prefix: &IpPrefix<A>) -> Option<V>`
- [ ] Walk to the node at the prefix's depth, take the value out, return it
- [ ] Prune any now-childless, now-valueless nodes on the way back up

**Why this matters:** Removal is the trickiest operation. Taking the value out is
easy — pruning is the interesting part. If a node has no value and no children after
removal, it is dead weight and should be deleted. But you can only know this on the
way *back up* the tree (after recursing), so this is a natural fit for a recursive
or post-order approach.

---

### 4.2 `contains(prefix: &IpPrefix<A>) -> bool`
- [x] Walk to the node at the prefix's depth, return whether it holds a value

**Why this matters:** Cheaper than `longest_match` when you only need a membership
check — no need to return a value or track a best-match register.

---

### 4.3 `iter()` — iterate all `(IpPrefix<A>, &V)` pairs

> **Deferred to Phase 7.** Building `iter()` now and rebuilding it after the
> treebitmap rewrite (Phase 6) is pure churn. Implement it once on the final layout.

**Why this matters:** Callers need to be able to inspect the full table — for
serialization, debugging, or feeding results into an `ipnetx` `IpSetBuilder`.
Reconstructing the prefix during traversal is non-obvious: you maintain a running
address and a depth counter, setting bits as you descend.

---

## Phase 5 — Benchmarks (Baseline)

Set up Criterion now, before the treebitmap rewrite. You need a baseline to prove
Phase 6 was worth the effort — numbers without a before/after comparison tell you
nothing.

### 5.1 Criterion setup
- [ ] Add `criterion` as a dev-dependency
- [ ] Create `benches/lookup.rs` with benchmarks for `insert` and `longest_match`
      at 1 000 / 10 000 / 100 000 prefixes for both IPv4 and IPv6
- [ ] Use realistic prefixes — generate them from real BGP table dumps or a seeded
      RNG, not sequential addresses, so the trie shape reflects real-world workloads
- [ ] Record baseline numbers from the current binary trie and save them somewhere
      (a comment in the bench file is fine)

**Why this matters:** Criterion gives you statistically rigorous measurements —
it runs each benchmark enough times to account for noise, reports confidence
intervals, and detects regressions automatically. The baseline here becomes the
control group for the treebitmap comparison in Phase 6.

---

## Phase 6 — Treebitmap

This is the performance rewrite. The current implementation is a 1-bit-at-a-time
binary trie: a `/24` prefix sits 24 node-hops deep, and every lookup walks up to
24 nodes. Treebitmap (Eatherton, Varghese, Bhatt — 2004) collapses that by
processing multiple bits per node, called a *stride*. With a stride of 4, a `/24`
only needs 6 hops. With a stride of 8, just 3.

The speedup is not just hop count. Each node encodes its children and internal
prefix matches as packed bitmaps, so the node itself is small and cache-friendly.
The data structure was designed specifically to fit routing tables in L2/L3 cache.

### 6.1 Understand the algorithm

The core insight is that a node no longer represents a single bit position — it
represents a *chunk* of `stride` bits. A stride-4 node covers 4 bits at once,
meaning it can have up to 2⁴ = 16 children and up to 2⁴ - 1 = 15 internal prefix
endpoints (prefixes that end *within* the node's bit range, not at its boundary).

Two bitmaps encode this compactly:

- **`internal` bitmap** — one bit per possible prefix endpoint *inside* the node.
  For stride 4, this covers prefixes of length 0–3 relative to the node's start.
  Bit is set if a prefix ends there. Popcount of bits ≤ position gives the value
  index in the node's value array.
- **`external` bitmap** — one bit per possible child pointer (2^stride of them).
  Bit is set if a child node exists for that sub-prefix. Same popcount trick gives
  the child index in the node's child array.

Both arrays are stored compactly — only set bits consume space — using the standard
popcount-rank trick:

```
index_of(bit N) = popcount(bitmap & ((1 << N) - 1))
```

This means a node with 3 children stores exactly 3 child pointers, not 16.

### 6.2 Choose a stride
- Stride 4 is the most common choice: nodes cover 4 bits, fit well in cache lines,
  and keep the bitmap math manageable (16-bit bitmaps).
- Stride 8 is faster but nodes are larger (256-entry bitmaps) and more complex.
- Start with stride 4.

### 6.3 Implementation steps
- [ ] Read the original paper or a detailed write-up before touching any code.
      The `hroi/treebitmap` source is a good reference once the algorithm is clear.
- [ ] Define a `TbNode<V>` with: `internal: u32` bitmap, `external: u32` bitmap,
      `values: Vec<V>`, `children: Vec<TbNode<V>>` (or arena indices)
- [ ] Implement `insert` — walk stride-aligned chunks of the prefix, creating nodes
      as needed, then set the correct `internal` bit and insert the value at the
      rank-computed position in `values`
- [ ] Implement `longest_match` — at each node, check the `internal` bitmap for any
      prefix that covers the current address chunk and record the best match; follow
      the `external` bitmap to the next child
- [ ] Implement `remove` — clear the `internal` bit and remove the value from `values`
      at the rank position; prune childless, valueless nodes
- [ ] Implement `contains`
- [ ] Run the Phase 5 benchmarks against the treebitmap implementation and record
      the comparison

**What to watch for:** The rank/popcount index arithmetic is the core operation and
the most likely source of off-by-one bugs. Test it in isolation before wiring it
into insert and lookup. The `u32::count_ones()` intrinsic compiles to a single
`POPCNT` instruction on x86 and ARM — it is fast.

**Key difference from `hroi/treebitmap`:** That crate uses unsafe code and manual
memory layout for maximum performance. Start with safe Rust and `Vec` — get the
algorithm correct first, then consider unsafe optimizations only if the benchmarks
demand it.

---

## Phase 7 — Quality and Polish

The crate works correctly and is fast. Now make it production-ready.

### 7.1 `iter()` — iterate all `(IpPrefix<A>, &V)` pairs
- [ ] Implement an in-order traversal of the trie that reconstructs each prefix
      from the path taken to reach its node

**Why this matters:** Callers need to be able to inspect the full table — for
serialization, debugging, or feeding results into an `ipnetx` `IpSetBuilder`.
With the treebitmap layout, traversal also needs to decode internal bitmap positions
back into prefix lengths — work through that carefully.

---

### 7.2 Standard trait implementations
- [ ] `impl Default for IpTable<A, V>`
- [ ] `impl Debug for IpTable<A, V> where V: Debug`
- [ ] `impl FromIterator<(IpPrefix<A>, V)> for IpTable<A, V>` — allows `collect()` into a table

---

### 7.3 Documentation
- [ ] Verify doc comments on all public types and methods each have at least one
      `# Example` block (these become doctests — they run with `cargo test`)
- [ ] Add `#[doc = include_str!("../README.md")]` to `lib.rs` so docs.rs renders the README

---

### 7.4 `serde` support
- [ ] Add `serde` as an optional dependency behind a `serde` feature flag
- [ ] Implement `Serialize` / `Deserialize` for `IpTable<A, V> where V: Serialize + DeserializeOwned`
- [ ] Add a feature-gated doc example showing round-trip through `serde_json`

**What to watch for:** Serialize as a flat list of `{ prefix, value }` records and
rebuild via `FromIterator` on deserialization. Do not serialize the internal trie
structure — that would couple the wire format to the implementation and break if
the layout ever changes.

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
1.1 Cargo metadata                         ✓
1.2 Understand IpPrefix<A> and bit extraction ✓
    ↓
2.1 Define TrieNode<V>                     ✓
    ↓
3.1 Implement insert                       ✓
3.2 Implement longest_match                ✓
    ↓
4.1 Implement remove                       ✓
4.2 Implement contains                     ✓
4.3 Implement iter                         → deferred to 7.1
    ↓
5.1 Criterion benchmarks (binary trie baseline)
    ↓
6.1 Understand treebitmap algorithm
6.2 Choose stride
6.3 Implement treebitmap + benchmark comparison
    ↓
7.1 iter()
7.2 Trait impls
7.3 Documentation
7.4 serde support
7.5 Publish
```
