# routemap

Fast, in-memory **Longest Prefix Match (LPM)** routing tables for IPv4 and IPv6 in Rust.

[![Crates.io](https://img.shields.io/crates/v/routemap.svg)](https://crates.io/crates/routemap)
[![Docs.rs](https://docs.rs/routemap/badge.svg)](https://docs.rs/routemap/latest/routemap/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![codecov](https://codecov.io/gh/dbrucknr/routemap/graph/badge.svg)](https://codecov.io/gh/dbrucknr/routemap)

```toml
[dependencies]
routemap = "0.1"
```

---

## Quick Start

```rust
use routemap::RouteMap;
use std::net::Ipv4Addr;

let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();

table.insert("10.0.0.0/8".parse().unwrap(),    "datacenter");
table.insert("10.20.0.0/16".parse().unwrap(),  "third-floor");
table.insert("0.0.0.0/0".parse().unwrap(),     "default");

// Most specific match always wins.
assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()),  Some(&"third-floor"));
assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()),  Some(&"datacenter"));
assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"default"));
assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()),  Some(&"third-floor"));
```

For IPv6, swap `Ipv4Addr` for `Ipv6Addr` and use IPv6 CIDR notation — everything else is identical.

---

## What is Longest Prefix Match?

When a router receives a packet for `192.168.1.57`, its routing table might contain several entries that all technically match:

```text
0.0.0.0/0       → default gateway
192.168.0.0/16  → office network
192.168.1.0/24  → third-floor subnet
```

All three cover `192.168.1.57`. The rule is: **the most specific prefix wins** — the one with the longest prefix length. That is `192.168.1.0/24` (length 24 beats length 16 beats length 0).

This is Longest Prefix Match. It is the core lookup rule behind:

- IP routing (every hardware and software router on the internet)
- Firewall ACL evaluation
- BGP route selection and RIB queries
- GeoIP lookups (which country or ASN owns this address?)
- Threat intelligence (is this IP in a known-bad prefix?)
- Traffic classification and network observability

---

## Use Cases

**Software routers and network daemons.** Any process that needs to forward or classify packets by destination — userspace routers, VPN daemons, SDN data planes, traffic shapers. These workloads build a table once from a configuration or BGP feed and then perform millions of lookups per second. The treebitmap's lookup throughput (25–47 M/s at 100k prefixes) is designed exactly for this shape.

**Firewall and ACL evaluation.** A firewall rule set is an LPM table where the value is a policy action (allow, deny, rate-limit). The most specific matching prefix wins — the same rule as routing. `routemap` evaluates the table in one pass with no per-lookup allocation, which matters when rules are evaluated on every packet.

**GeoIP and ASN classification.** Mapping arbitrary IP addresses to countries, regions, or autonomous systems means maintaining a table of tens of thousands of IP prefixes and looking up each inbound request. The table is loaded once at startup and read continuously — again, the treebitmap's read-heavy profile fits well.

**Threat intelligence and IP reputation.** Blocklists and allowlists are typically expressed as IP prefixes (known Tor exit nodes, cloud provider ranges, known-bad ASNs). Loading these into a table and classifying traffic at the edge is a direct application.

**Traffic observability and classification.** Tagging network flows by segment — "this connection is from the office VPN range", "this source is in the datacenter /16" — for dashboards, billing, or anomaly detection. The value stored alongside each prefix can be anything: a string label, an enum, a numeric tenant ID.

**Cloud infrastructure tooling.** VPC route table simulation, classifying whether a request originates from a cloud provider's IP range, Kubernetes network policy enforcement — these are all LPM problems that appear at the infrastructure layer.

---

## Why Not a HashMap?

A `HashMap<IpAddr, Route>` can only tell you whether an exact IP address was inserted. It cannot answer "which of my prefixes contains this address?" — because a prefix like `10.0.0.0/8` is not a single address, it is a description of 16 million addresses.

To simulate LPM with a hash map you would need to probe all 32 possible prefix lengths for IPv4 (or 128 for IPv6) on every lookup, rebuild the prefix from the address at each length, and then pick the longest match yourself. That is O(W) hash lookups with high constant overhead and no spatial locality.

`routemap` answers the question directly, with one pass and no per-lookup allocation.

---

## The Full API

### `RouteMap<A, V>`

The table type has two type parameters:

- **`A`** — the address family. Use `std::net::Ipv4Addr` or `std::net::Ipv6Addr`. A single table is dedicated to one family; use two tables if you need both.
- **`V`** — whatever you want to store alongside each prefix: route entries, ASN numbers, country codes, rule IDs, anything.

```rust
use routemap::RouteMap;
use std::net::Ipv4Addr;

let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
```

### `insert(prefix, value)`

Adds or replaces the entry for a prefix. Host bits in the address are silently ignored — `10.99.0.0/8` and `10.0.0.0/8` are the same prefix.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
table.insert("10.0.0.0/8".parse().unwrap(), 42);
table.insert("10.0.0.0/8".parse().unwrap(), 99); // replaces 42
```

### `longest_match(addr) -> Option<&V>`

Returns a shared reference to the value for the most specific matching prefix, or `None` if no prefix covers the address.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
# table.insert("10.0.0.0/8".parse().unwrap(), 99);
assert_eq!(table.longest_match("10.1.2.3".parse().unwrap()), Some(&99));
assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
```

### `contains(prefix) -> bool`

Returns `true` if the exact prefix was inserted. This is an exact match, not a longest-prefix match — it does not return `true` just because an address within the prefix would match via `longest_match`.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
table.insert("10.0.0.0/8".parse().unwrap(), 1);

assert!(table.contains("10.0.0.0/8".parse().unwrap()));   // exact match — true
assert!(!table.contains("10.20.0.0/16".parse().unwrap())); // never inserted — false
```

### `remove(prefix) -> Option<V>`

Removes a prefix and returns its value. Returns `None` if the prefix was not in the table. Removing a broad prefix does not affect more specific prefixes nested beneath it.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
table.insert("10.20.0.0/16".parse().unwrap(), "specific");

table.remove("10.0.0.0/8".parse().unwrap()); // removes "broad"

// "specific" is still there; 10.0.x.x outside /16 now returns None
assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"specific"));
assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), None);
```

### `len() -> usize` and `is_empty() -> bool`

`len` returns the number of prefix entries in the table in O(1) time. `is_empty` returns `true` when the table has no entries.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
assert!(table.is_empty());

table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
table.insert("10.20.0.0/16".parse().unwrap(), "specific");
assert_eq!(table.len(), 2);

// Overwriting an existing prefix does not change the count.
table.insert("10.0.0.0/8".parse().unwrap(), "updated");
assert_eq!(table.len(), 2);

table.remove("10.0.0.0/8".parse().unwrap());
assert_eq!(table.len(), 1);
```

### `iter()` and `for (prefix, value) in &table`

Returns an iterator over all `(IpPrefix<A>, &V)` pairs. The table also implements `IntoIterator` for shared references, so standard `for` loop syntax works directly.

```rust
# use routemap::RouteMap;
# use std::net::Ipv4Addr;
# let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
table.insert("10.20.0.0/16".parse().unwrap(), "specific");

// for-loop syntax via IntoIterator
for (prefix, value) in &table {
    let _ = (prefix, value); // prefix: IpPrefix<Ipv4Addr>, value: &&str
}

// Collect all entries into a Vec, sorted by prefix length
let mut entries: Vec<_> = table.iter().collect();
entries.sort_by_key(|(p, _)| p.mask());
assert_eq!(entries[0].1, &"broad");    // /8 comes first
assert_eq!(entries[1].1, &"specific"); // /16 comes second
```

---

## Performance

Backed by a **stride-4 treebitmap**. Instead of following one bit per node (as a binary trie does), each node processes 4 bits at once. This cuts maximum lookup depth from 32 → 8 hops for IPv4 and 128 → 32 hops for IPv6, and keeps related prefixes in the same cache line.

Benchmarked on Apple M2 Max, Rust 1.94.0, `cargo bench`. All throughput
figures are in **M/s — millions of operations per second**.

### Lookup — `longest_match` throughput

| prefixes | IPv4 | IPv6 |
|---:|---:|---:|
| 1,000 | 89 M/s | 82 M/s |
| 10,000 | 38 M/s | 65 M/s |
| 100,000 | 25 M/s | 47 M/s |

IPv6 throughput *increases* relative to IPv4 at scale because the treebitmap's stride savings are proportionally larger: a binary trie would make up to 128 hops for a /128 prefix, the treebitmap caps this at 32.

### Insert — table build throughput

| prefixes | IPv4 | IPv6 |
|---:|---:|---:|
| 1,000 | 15 M/s | 4.2 M/s |
| 10,000 | 14 M/s | 4.3 M/s |
| 100,000 | 14 M/s | 3.5 M/s |

### The trade-off

Insert is the weak side. Each node stores values and children in compact `Vec`s (no empty slots), which is what keeps lookup cache-friendly. But maintaining that compact layout on insert requires shifting elements — a `Vec::insert()` per affected node. At small table sizes (< 10k prefixes) this makes `insert` roughly 2× slower than a plain binary trie.

The cost converges at large sizes: with a realistic BGP-shaped prefix distribution, each treebitmap node holds close to one value on average, so the shift cost essentially disappears at 100k prefixes.

**Bottom line: if your workload is lookup-heavy (routing, firewall evaluation, GeoIP), the treebitmap wins. If your table is small and mutates constantly, the binary trie is faster for the insert path.**

---

## How It Works

A binary trie walks the address one bit at a time — each lookup on a `/24` makes 24 pointer dereferences into separately-allocated nodes. At 100k prefixes those nodes are spread across the heap, and nearly every dereference is a cache miss.

The treebitmap (Eatherton, Varghese, Bhatt — 2004) processes 4 bits per node (a *stride* of 4). Each node holds two bitmaps and two compact `Vec`s:

- **Internal bitmap** (15 bits) — one bit for each prefix that *ends within* this node's 4-bit window, indexed by a binary-heap position formula.
- **External bitmap** (16 bits) — one bit for each possible next nibble (0–15) that has a child node.
- **`values`** — compact `Vec`; length equals `internal.count_ones()`.
- **`children`** — compact `Vec`; length equals `external.count_ones()`.

Finding a value or child in the `Vec` uses a single `POPCNT` instruction:

```text
index = (bitmap & ((1 << position) - 1)).count_ones()
```

A lookup for `10.20.5.1` across a full IPv4 table visits at most 8 nodes instead of 32. Each of those 8 nodes checks up to 4 internal positions (the prefixes that end within its stride window) in a tight loop before following the external pointer.

See [`treebitmap.md`](treebitmap.md) at the repo root for a step-by-step walkthrough with concrete bitmaps.

---

## Relationship to `ipnetx`

`routemap` uses [`IpPrefix<A>`](https://crates.io/crates/ipnetx) from the `ipnetx` crate as its key type. You do not need to know the internals of `ipnetx` to use `routemap` — CIDR string parsing is all you need:

```rust
# use ipnetx::prefix::IpPrefix;
# use std::net::Ipv4Addr;
let prefix: IpPrefix<Ipv4Addr> = "10.0.0.0/8".parse().unwrap();
```

The two crates answer different questions and are designed to compose:

- **`ipnetx`** reasons about *regions* of IP address space as mathematical sets — union, intersection, difference, complement. Use it to build, validate, and manipulate collections of prefixes.
- **`routemap`** classifies *individual addresses* against a table of prefixes at lookup speed. Use it to answer "which rule covers this packet?" at runtime.

A typical pipeline: use `ipnetx` to aggregate and deduplicate a raw prefix list (collapsing overlapping or adjacent entries), then load the result into an `routemap` table for classification. `ipnetx` handles the set algebra once at build time; `routemap` handles the per-packet lookups at runtime.

---

## Common Mistakes

**Inserting a prefix with host bits set.** `10.99.0.0/8` and `10.0.0.0/8` are the same prefix — `insert` and `remove` both call `.masked()` internally to zero the host bits before operating. You will not get duplicates or miss entries because of this, but the canonical form stored is always the masked one.

**Expecting `contains` to behave like `longest_match`.** `contains("10.20.0.0/16")` returns `false` if that exact prefix was never inserted, even if `longest_match("10.20.5.1")` would return a value via a covering `/8`. Use `contains` to answer "did I insert this exact prefix?" and `longest_match` to answer "what prefix covers this address?"

**Using separate tables for IPv4 and IPv6 is correct and intentional.** The type parameter `A` is enforced at compile time — you cannot accidentally insert an IPv6 prefix into an IPv4 table.

---

## License

MIT
