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

*To be filled in after Phase 6.*

### Insert

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | | | | |
| 10,000 | | | | |
| 100,000 | | | | |

### Lookup

| prefixes | IPv4 time | IPv4 thrpt | IPv6 time | IPv6 thrpt |
|---:|---:|---:|---:|---:|
| 1,000 | | | | |
| 10,000 | | | | |
| 100,000 | | | | |

---

## Comparison (binary trie → treebitmap)

*To be filled in after Phase 6.*

### Lookup speedup

| prefixes | IPv4 | IPv6 |
|---:|---:|---:|
| 1,000 | | |
| 10,000 | | |
| 100,000 | | |
