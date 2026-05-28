# netlpm

Longest Prefix Match (LPM) routing tables for Rust, built on [`ipnetx`](https://crates.io/crates/ipnetx).

```toml
[dependencies]
netlpm = "0.1"
```

---

## What is Longest Prefix Match?

When a router receives a packet destined for `192.168.1.57`, its routing table might contain several overlapping entries:

```
0.0.0.0/0      → default gateway
10.0.0.0/8     → datacenter backbone
192.168.0.0/16 → office network
192.168.1.0/24 → third-floor subnet
```

`192.168.1.57` matches three of those entries simultaneously. The rule is always: **the most specific one wins** — the prefix with the longest prefix length. That is `192.168.1.0/24`. This is Longest Prefix Match, and it is the core lookup operation behind:

- IP routing (every hardware and software router)
- Firewall ACL evaluation (most specific rule wins)
- BGP route selection and RIB queries
- GeoIP lookups (which country or ASN owns this address?)
- Threat intelligence (is this IP in a known-bad prefix?)
- Traffic classification and network observability

---

## Why a Dedicated Data Structure?

The global BGP routing table contains roughly 900,000 prefixes today. A busy router may need millions of LPM lookups per second. Naive approaches break down fast:

**Linear scan** checks every prefix in order — O(n) per lookup. 900,000 comparisons per packet is not viable.

**Hash map** cannot directly express LPM. You would need to probe all 32 possible prefix lengths (for IPv4) per lookup, and the result still does not give you "most specific match" cleanly.

**Sorted array + binary search** requires careful ordering to express prefix specificity, and extracting the longest match from overlapping ranges requires extra bookkeeping.

None of these naturally express the question: *what is the deepest matching node in the prefix hierarchy?*

---

## How It Works: Patricia Trie

`netlpm` uses a **Patricia trie** (also called a compressed radix tree). A trie is a tree where the path from root to a node spells out the key — for IP prefixes, the key is the binary representation of the address.

Consider a table with two entries:

```
10.0.0.0/8   → RouteA
10.20.0.0/16 → RouteB
```

A lookup for `10.20.5.1` (binary: `00001010 00010100 00000101 00000001`) works like this:

1. Start at the root. Walk bit by bit.
2. After 8 bits (`00001010`) we have traced `10.x.x.x`. A prefix is stored here — record **RouteA** as the best match so far.
3. After 16 more bits (`00010100`) we have traced `10.20.x.x`. A prefix is stored here — update best match to **RouteB**.
4. No further matches. Return **RouteB** — the longest (most specific) match.

LPM is literally *deepest match while walking the trie*. The data structure encodes the problem directly.

**Compression** is what makes this practical. A naive binary trie for IPv4 has depth 32 — one node per bit — wasting memory on long single-child chains. A Patricia trie collapses those chains into single edges labeled with the skipped bits. Memory usage is proportional to the number of stored prefixes, not to the address width.

The result:

| Property | Value |
|---|---|
| Lookup | O(W) — at most 32 steps for IPv4, 128 for IPv6 |
| Insert | O(W) |
| Memory | O(n) — proportional to number of prefixes |
| Table-size independence | A 10-prefix and 1,000,000-prefix table have the same worst-case lookup cost |

W is a constant (32 or 128), so lookups are effectively O(1) in practice regardless of table size.

---

## Quick Start

```rust
use netlpm::IpTable;

let mut table: IpTable<&str> = IpTable::new();

table.insert("10.0.0.0/8".parse()?,   "datacenter");
table.insert("10.20.0.0/16".parse()?, "third-floor");
table.insert("0.0.0.0/0".parse()?,    "default");

// Longest prefix match
let result = table.longest_match("10.20.5.1".parse()?);
assert_eq!(result.map(|(_, v)| *v), Some("third-floor"));

// Falls back to less-specific match
let result = table.longest_match("10.99.0.1".parse()?);
assert_eq!(result.map(|(_, v)| *v), Some("datacenter"));
```

---

## Benchmarks

*Coming soon — benchmarked on MacBook Pro M2 Max.*

| Operation | 100 prefixes | 1 000 prefixes | 10 000 prefixes | Takeaway |
|---|---|---|---|---|
| `longest_match` (IPv4) | — | — | — | |
| `insert` (IPv4) | — | — | — | |
| `longest_match` (IPv6) | — | — | — | |

---

## Relationship to `ipnetx`

`netlpm` uses [`IpPrefix<A>`](https://docs.rs/ipnetx) from `ipnetx` as its key type. `ipnetx` provides set algebra on IP address space (union, intersection, difference, complement); `netlpm` provides the lookup table. They are designed to complement each other:

- Use `ipnetx` when you need to reason about *regions* of IP space as mathematical sets.
- Use `netlpm` when you need to *classify an individual IP* against a set of prefixes at lookup speed.

---

## License

MIT
