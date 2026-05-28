# netlpm — Implementation Plan

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
- [ ] Walk to the node at the prefix's depth, return whether it holds a value

**Why this matters:** Cheaper than `longest_match` when you only need a membership
check — no need to return a value or track a best-match register.

---

### 4.3 `iter()` — iterate all `(IpPrefix<A>, &V)` pairs
- [ ] Implement an in-order traversal of the trie that reconstructs each prefix
      from the path taken to reach its node

**Why this matters:** Callers need to be able to inspect the full table — for
serialization, debugging, or feeding results into an `ipnetx` `IpSetBuilder`.
Reconstructing the prefix during traversal is non-obvious: you maintain a running
address and a depth counter, setting bits as you descend.

---

## Phase 5 — Quality and Polish

The crate works correctly. Now we make it production-ready.

### 5.1 Standard trait implementations
- [ ] `impl Default for IpTable<V>`
- [ ] `impl Debug for IpTable<V> where V: Debug`
- [ ] `impl FromIterator<(IpPrefix<A>, V)> for IpTable<V>` — allows `collect()` into a table

---

### 5.2 Documentation
- [ ] Write doc comments on all public types and methods with at least one `# Example`
      block each (these become doctests — they run with `cargo test`)
- [ ] Add `#[doc = include_str!("../README.md")]` to `lib.rs` so docs.rs renders the README

---

### 5.3 Benchmarks
- [ ] Add `criterion` as a dev-dependency
- [ ] Write benchmarks for `insert` and `longest_match` at 100 / 1 000 / 10 000 prefixes
      for both IPv4 and IPv6
- [ ] Run on MacBook Pro M2 Max and fill in the README benchmark table

---

### 5.4 Publish
- [ ] `cargo test` — all tests pass
- [ ] `cargo doc --open` — documentation looks correct
- [ ] `cargo publish --dry-run` — no packaging errors
- [ ] `cargo publish`
- [ ] Update the `ipnetx` README to cross-reference `netlpm`

---

## Phase Order Summary

```
1.1 Cargo metadata
1.2 Understand IpPrefix<A> and bit extraction
    ↓
2.1 Define TrieNode<V>
    ↓
3.1 Implement insert
3.2 Implement longest_match
    ↓
4.1 Implement remove
4.2 Implement contains
4.3 Implement iter
    ↓
5.1 Trait impls
5.2 Documentation
5.3 Benchmarks
5.4 Publish
```
