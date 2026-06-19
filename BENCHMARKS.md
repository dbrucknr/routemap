# Benchmarks

Prefix length distributions are weighted toward real-world BGP table shapes:
IPv4 favors /16–/28, IPv6 favors /48–/64. Lookup addresses use a separate random
seed from inserted prefixes to produce a realistic mix of hits and misses.

All results collected with `cargo bench` (Criterion 0.8, release profile).

**Environment**

| | |
|---|---|
| CPU | Apple M2 Max |
| Memory | 32 GB |
| Rust | 1.94.0 (Homebrew) |
| OS | macOS |

---

## Binary trie (arena-based)

*Phase 4 implementation — baseline before Phase 6 (treebitmap) rewrite.*

### Insert — time to build a fresh table from N prefixes

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 31.2 µs | 32.0 M/s | 108 µs | 9.2 M/s |
| 10,000 | 526 µs | 19.0 M/s | 1.80 ms | 5.6 M/s |
| 100,000 | 7.14 ms | 14.0 M/s | 25.8 ms | 3.9 M/s |

### Lookup — `longest_match` throughput on a pre-built table

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 14.3 µs | 69.9 M/s | 17.8 µs | 56.2 M/s |
| 10,000 | 419 µs | 23.8 M/s | 508 µs | 19.7 M/s |
| 100,000 | 5.37 ms | 18.6 M/s | 7.45 ms | 13.4 M/s |

### Notes

- IPv6 insert is ~3.5x slower than IPv4 at 100k prefixes — each prefix walks up
  to 128 nodes vs. 32 for IPv4.
- Lookup throughput drops sharply from 1k → 10k (70M/s → 24M/s for IPv4). At 1k
  the trie fits in L1/L2 cache; at 100k most node accesses are L3 misses. This is
  the primary motivation for the treebitmap rewrite.

---

## Treebitmap (stride-4)

*Phase 6 implementation. Recorded 2026-05-29 on Apple M2 Max, Rust 1.94.0.*

### Insert — time to build a fresh table from N prefixes

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 68.4 µs | 14.6 M/s | 237.8 µs | 4.2 M/s |
| 10,000 | 692.7 µs | 14.4 M/s | 2.34 ms | 4.3 M/s |
| 100,000 | 6.97 ms | 14.3 M/s | 28.5 ms | 3.5 M/s |

### Lookup — `longest_match` throughput on a pre-built table

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 11.2 µs | 89.3 M/s | 12.1 µs | 82.4 M/s |
| 10,000 | 261.2 µs | 38.3 M/s | 153.6 µs | 65.1 M/s |
| 100,000 | 3.98 ms | 25.1 M/s | 2.11 ms | 47.4 M/s |

### Notes

- Insert is 2–2.2× slower than the binary trie at small sizes. Treebitmap inserts
  use `Vec::insert()` to maintain compact sorted value/child arrays, which costs
  more than the binary trie's simple node allocation at small scales.
- At 100k prefixes, insert cost converges: IPv4 is within ~1% of the binary trie
  because the per-node value count stays near 1 in a dense real-world prefix
  distribution, making the `Vec::insert()` cost negligible.
- IPv6 insert remains 10–20% slower than binary trie at 100k due to the deeper
  recursion (32 strides vs. 8), but is much more competitive than at smaller sizes.
- Lookup is uniformly faster. The stride-4 structure cuts maximum hop count from
  32 to 8 for IPv4 and from 128 to 32 for IPv6, directly reducing cache pressure.

---

## Treebitmap (stride-4) — pre-optimization baseline

*Recorded 2026-06-19 on Apple M2 Max, Rust 1.86.0. Pre-optimization baseline before
inline hints, iterative traversal, and iterator stack pre-allocation.*

### Insert — time to build a fresh table from N prefixes

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 68.97 µs | 14.50 M/s | 237.56 µs | 4.21 M/s |
| 10,000 | 696.61 µs | 14.36 M/s | 2.37 ms | 4.23 M/s |
| 100,000 | 7.06 ms | 14.16 M/s | 28.36 ms | 3.53 M/s |

### Lookup — `longest_match` throughput on a pre-built table

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | 11.33 µs | 88.26 M/s | 12.08 µs | 82.80 M/s |
| 10,000 | 258.21 µs | 38.73 M/s | 152.08 µs | 65.75 M/s |
| 100,000 | 4.01 ms | 24.97 M/s | 2.11 ms | 47.32 M/s |

---

## Comparison (binary trie → treebitmap)

*Speedup is the ratio of throughput: treebitmap M/s ÷ binary trie M/s.*

### Insert throughput change

| prefixes | IPv4 | IPv6 |
|---:|---:|---:|
| 1,000 | −54% (2.2× slower) | −54% (2.2× slower) |
| 10,000 | −24% (1.3× slower) | −23% (1.3× slower) |
| 100,000 | +2% (≈ same) | −10% (1.1× slower) |

### Lookup speedup

| prefixes | IPv4 | IPv6 |
|---:|---:|---:|
| 1,000 | 1.28× | 1.47× |
| 10,000 | 1.61× | 3.30× |
| 100,000 | 1.35× | 3.54× |

### Analysis

The lookup gains are clearest where cache pressure dominates — the 10k→100k range:

- **IPv4 lookup** improves 1.3–1.6×. IPv4 prefixes use only 8 strides, so the
  binary trie was already reasonably shallow (max 32 hops). The treebitmap reduces
  this to max 8 hops and checks up to 4 internal positions per hop, trading a
  few extra comparisons per node for dramatically fewer node visits.

- **IPv6 lookup** improves 3.3–3.5× at scale. The binary trie makes up to 128
  pointer hops for a /128; treebitmap caps this at 32. At 100k prefixes, where
  node accesses are almost entirely L3 cache misses, cutting hops by 4× translates
  almost directly to a 3.5× wall-clock speedup.

- **Insert regression** at small sizes is the main cost. `Vec::insert()` requires
  shifting elements to maintain the compact representation. In practice, lookup
  workloads vastly outnumber insert workloads in routing software, so this
  tradeoff is strongly in favour of treebitmap for any read-heavy use case.
