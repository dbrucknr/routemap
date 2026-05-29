use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::marker::PhantomData;

mod node;
use node::TbNode;

const STRIDE: u32 = 4;

/// Returns the nibble (4-bit value) at stride index `hop` within `addr`.
/// `addr_bits` is A::BITS (32 for IPv4, 128 for IPv6).
fn nibble(addr: u128, addr_bits: u32, hop: u32) -> u32 {
    let shift = addr_bits - (hop + 1) * STRIDE;
    ((addr >> shift) & 0xF) as u32
}

/// Counts how many set bits in `bitmap` fall below `position`.
/// Maps a bitmap position to a compact-Vec index via a single POPCNT.
fn rank(bitmap: u32, position: u32) -> usize {
    (bitmap & ((1 << position) - 1)).count_ones() as usize
}

/// An in-memory Longest Prefix Match (LPM) routing table backed by a stride-4
/// treebitmap. Each node processes 4 bits at once instead of 1, which cuts the
/// maximum number of pointer hops from 32 → 8 (IPv4) and 128 → 32 (IPv6) and
/// dramatically reduces cache pressure on large tables.
///
/// An LPM table maps IP network prefixes — like `10.0.0.0/8` or `192.168.1.0/24` —
/// to values of any type `V`. When you look up an IP address, the table returns the
/// value associated with the *most specific* matching prefix.
///
/// # Type parameters
///
/// - `A` — the address family: [`Ipv4Addr`](std::net::Ipv4Addr) or
///   [`Ipv6Addr`](std::net::Ipv6Addr). A single table is dedicated to one family.
/// - `V` — the value stored alongside each prefix.
///
/// # Performance
///
/// **Lookup** is the primary strength. At 100k prefixes on an M2 Max:
///
/// | | IPv4 | IPv6 |
/// |---|---|---|
/// | Throughput | 25 M lookups/s | 47 M lookups/s |
/// | vs. binary trie | 1.35× faster | 3.54× faster |
///
/// IPv6 benefits most: the binary trie makes up to 128 pointer hops for a /128;
/// the treebitmap caps this at 32. Each hop is a potential cache miss at scale,
/// so the 4× reduction in hops translates almost directly to a 3.5× speedup.
///
/// **Insert** has a cost: each node stores values and children in compact `Vec`s
/// (no empty slots), so inserting into a populated node requires a `Vec::insert()`
/// to shift elements. At small table sizes this makes insert ~2× slower than a
/// binary trie. The cost converges to parity around 100k prefixes, where the
/// average number of values per node approaches 1 and shifts are rare.
///
/// **Use this table when lookups dominate.** If your workload is insert-heavy and
/// tables stay small (< 10k prefixes), a plain binary trie may be faster overall.
///
/// # Example
///
/// ```
/// use iplookup::IpTable;
/// use std::net::Ipv4Addr;
///
/// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
/// table.insert("10.0.0.0/8".parse().unwrap(), "datacenter");
/// table.insert("10.20.0.0/16".parse().unwrap(), "third-floor");
///
/// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"third-floor"));
/// assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"datacenter"));
/// assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
/// ```
pub struct IpTable<A: IpAddress, V> {
    root: TbNode<V>,
    _marker: PhantomData<A>,
}

impl<A: IpAddress, V> IpTable<A, V> {
    /// Creates a new, empty LPM table.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// ```
    pub fn new() -> Self {
        Self {
            root: TbNode::new(),
            _marker: PhantomData,
        }
    }

    /// Inserts a prefix and its associated value into the table.
    ///
    /// If an entry already exists for this prefix, its value is replaced.
    /// Host bits in the prefix address are ignored — `10.99.0.0/8` and
    /// `10.0.0.0/8` are treated as the same prefix.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
    /// table.insert("10.0.0.0/8".parse().unwrap(), "updated");
    /// assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"updated"));
    /// ```
    pub fn insert(&mut self, prefix: IpPrefix<A>, value: V) {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        insert_at(
            &mut self.root,
            addr,
            A::BITS as u32,
            0,
            len / STRIDE,
            len % STRIDE,
            value,
        );
    }

    /// Returns a reference to the value for the most specific matching prefix,
    /// or `None` if no prefix in the table matches `addr`.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// table.insert("0.0.0.0/0".parse().unwrap(), "default");
    /// table.insert("10.0.0.0/8".parse().unwrap(), "datacenter");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "third-floor");
    ///
    /// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"third-floor"));
    /// assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"datacenter"));
    /// assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"default"));
    /// ```
    pub fn longest_match(&self, addr: A) -> Option<&V> {
        let addr = addr.to_u128();
        let addr_bits = A::BITS as u32;
        let total_strides = addr_bits / STRIDE;
        let mut node = &self.root;
        let mut best: Option<&V> = None;
        let mut last_advanced = false;

        for hop in 0..total_strides {
            let nib = nibble(addr, addr_bits, hop);

            // Check all four relative lengths (0–3) inside this node's stride,
            // from least to most specific so the last hit wins.
            for rel_len in 0..STRIDE {
                let rel_v = if rel_len == 0 { 0u32 } else { nib >> (STRIDE - rel_len) };
                let bpos = (1u32 << rel_len) + rel_v;
                if (node.internal >> bpos) & 1 == 1 {
                    best = Some(&node.values[rank(node.internal, bpos)]);
                }
            }

            // Descend to the external child for the full nibble, or stop.
            if (node.external >> nib) & 1 == 0 {
                last_advanced = false;
                break;
            }
            node = &node.children[rank(node.external, nib)];
            last_advanced = true;
        }

        // If we advanced on the very last stride, the depth-(total_strides) node was
        // never visited by the loop body. Check its catch-all position (rel_len=0,
        // bpos=1) — the only position reachable when all address bits are consumed.
        // This handles /32 for IPv4 and /128 for IPv6.
        if last_advanced && (node.internal >> 1) & 1 == 1 {
            best = Some(&node.values[rank(node.internal, 1)]);
        }

        best
    }

    /// Removes the entry for `prefix` and returns its value, or `None` if not found.
    ///
    /// Host bits in the prefix address are ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "specific");
    ///
    /// assert_eq!(table.remove("10.20.0.0/16".parse().unwrap()), Some("specific"));
    /// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"broad"));
    /// ```
    pub fn remove(&mut self, prefix: IpPrefix<A>) -> Option<V> {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        let (value, _) = remove_at(
            &mut self.root,
            addr,
            A::BITS as u32,
            0,
            len / STRIDE,
            len % STRIDE,
        );
        value
    }

    /// Returns `true` if the table contains an exact entry for this prefix.
    ///
    /// This is an exact match — not a longest-prefix match. Host bits are ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "ten");
    ///
    /// assert!(table.contains("10.0.0.0/8".parse().unwrap()));
    /// assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    /// ```
    /// Returns an iterator over all `(prefix, &value)` pairs in the table.
    ///
    /// Entries are yielded in depth-first order: shorter prefixes at a given
    /// node before the longer prefixes stored in its children.
    ///
    /// # Example
    ///
    /// ```
    /// use iplookup::IpTable;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "specific");
    ///
    /// let mut entries: Vec<_> = table.iter().collect();
    /// entries.sort_by_key(|(p, _)| p.mask());
    ///
    /// assert_eq!(entries[0].1, &"broad");
    /// assert_eq!(entries[1].1, &"specific");
    /// ```
    pub fn iter(&self) -> Iter<'_, A, V> {
        Iter {
            stack: vec![IterFrame {
                node: &self.root,
                hop: 0,
                addr: 0,
                internal_cursor: 1,
                external_cursor: 0,
            }],
            addr_bits: A::BITS as u32,
            _marker: PhantomData,
        }
    }

    pub fn contains(&self, prefix: IpPrefix<A>) -> bool {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        contains_at(
            &self.root,
            addr,
            A::BITS as u32,
            0,
            len / STRIDE,
            len % STRIDE,
        )
    }
}

fn insert_at<V>(
    node: &mut TbNode<V>,
    addr: u128,
    addr_bits: u32,
    current_hop: u32,
    full_hops: u32,
    rel_len: u32,
    value: V,
) {
    if current_hop == full_hops {
        let nib = if rel_len > 0 {
            nibble(addr, addr_bits, current_hop) >> (STRIDE - rel_len)
        } else {
            0
        };
        let bpos = (1u32 << rel_len) + nib;
        let idx = rank(node.internal, bpos);
        if (node.internal >> bpos) & 1 == 1 {
            node.values[idx] = value;
        } else {
            node.values.insert(idx, value);
            node.internal |= 1 << bpos;
        }
    } else {
        let nib = nibble(addr, addr_bits, current_hop);
        let already_exists = (node.external >> nib) & 1 != 0;
        let child_idx = rank(node.external, nib);
        if !already_exists {
            node.children.insert(child_idx, TbNode::new());
            node.external |= 1 << nib;
        }
        insert_at(
            &mut node.children[child_idx],
            addr,
            addr_bits,
            current_hop + 1,
            full_hops,
            rel_len,
            value,
        );
    }
}

fn remove_at<V>(
    node: &mut TbNode<V>,
    addr: u128,
    addr_bits: u32,
    current_hop: u32,
    full_hops: u32,
    rel_len: u32,
) -> (Option<V>, bool) {
    if current_hop == full_hops {
        let nib = if rel_len > 0 {
            nibble(addr, addr_bits, current_hop) >> (STRIDE - rel_len)
        } else {
            0
        };
        let bpos = (1u32 << rel_len) + nib;

        if (node.internal >> bpos) & 1 == 0 {
            return (None, false);
        }

        let idx = rank(node.internal, bpos);
        let value = node.values.remove(idx);
        node.internal &= !(1u32 << bpos);

        let is_empty = node.internal == 0 && node.external == 0;
        (Some(value), is_empty)
    } else {
        let nib = nibble(addr, addr_bits, current_hop);

        if (node.external >> nib) & 1 == 0 {
            return (None, false);
        }

        let child_idx = rank(node.external, nib);
        let (value, prune) =
            remove_at(&mut node.children[child_idx], addr, addr_bits, current_hop + 1, full_hops, rel_len);

        if prune {
            node.children.remove(child_idx);
            node.external &= !(1u32 << nib);
        }

        let is_empty = node.internal == 0 && node.external == 0;
        (value, is_empty)
    }
}

fn contains_at<V>(
    node: &TbNode<V>,
    addr: u128,
    addr_bits: u32,
    current_hop: u32,
    full_hops: u32,
    rel_len: u32,
) -> bool {
    if current_hop == full_hops {
        let nib = if rel_len > 0 {
            nibble(addr, addr_bits, current_hop) >> (STRIDE - rel_len)
        } else {
            0
        };
        let bpos = (1u32 << rel_len) + nib;
        (node.internal >> bpos) & 1 == 1
    } else {
        let nib = nibble(addr, addr_bits, current_hop);
        if (node.external >> nib) & 1 == 0 {
            return false;
        }
        let child_idx = rank(node.external, nib);
        contains_at(
            &node.children[child_idx],
            addr,
            addr_bits,
            current_hop + 1,
            full_hops,
            rel_len,
        )
    }
}

// ── Iterator ──────────────────────────────────────────────────────────────────

struct IterFrame<'a, V> {
    node: &'a TbNode<V>,
    hop: u32,
    /// Accumulated address bits set so far by following nibbles from the root.
    addr: u128,
    /// Next internal bitmap position to scan (1..=15; set to 16 when exhausted).
    internal_cursor: u32,
    /// Next external bitmap nibble to scan (0..=15; set to 16 when exhausted).
    external_cursor: u32,
}

/// An iterator over all `(prefix, &value)` pairs in an [`IpTable`].
///
/// Entries are yielded in depth-first order: a node's internal prefixes
/// (shorter to longer within the stride) before its children's prefixes.
///
/// Created by [`IpTable::iter`].
pub struct Iter<'a, A: IpAddress, V> {
    stack: Vec<IterFrame<'a, V>>,
    addr_bits: u32,
    _marker: PhantomData<A>,
}

impl<'a, A: IpAddress, V> Iterator for Iter<'a, A, V> {
    type Item = (IpPrefix<A>, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let depth = self.stack.len();
            if depth == 0 {
                return None;
            }

            // Copy all fields we need out of the frame first.
            // &'a TbNode<V> is Copy, so this severs the borrow from self.stack,
            // letting us mutate self.stack (write cursor, push child) afterwards.
            let node: &'a TbNode<V> = self.stack[depth - 1].node;
            let hop = self.stack[depth - 1].hop;
            let addr = self.stack[depth - 1].addr;

            // ── Internal entries ──────────────────────────────────────────────
            // bpos encodes (rel_len, rel_bits) via binary-heap indexing:
            //   rel_len  = floor(log2(bpos))
            //   rel_bits = bpos - (1 << rel_len)
            let ic = self.stack[depth - 1].internal_cursor;
            if ic <= 15 {
                let above = node.internal >> ic;
                if above != 0 {
                    let bpos = ic + above.trailing_zeros();
                    self.stack[depth - 1].internal_cursor = bpos + 1;

                    let rel_len = 31 - bpos.leading_zeros(); // floor(log2(bpos))
                    let rel_bits = bpos - (1u32 << rel_len);
                    let full_len = hop * STRIDE + rel_len;
                    let full_addr = if rel_len > 0 {
                        addr | ((rel_bits as u128)
                            << (self.addr_bits - hop * STRIDE - rel_len))
                    } else {
                        addr
                    };

                    let idx = rank(node.internal, bpos);
                    let value: &'a V = &node.values[idx];
                    let prefix =
                        IpPrefix::new(A::from_u128(full_addr), full_len as u8).unwrap();
                    return Some((prefix, value));
                }
                self.stack[depth - 1].internal_cursor = 16;
            }

            // ── External children ─────────────────────────────────────────────
            // Returns true when a child was pushed so we skip the pop and loop
            // back to process the new top frame.  No `continue` is used to avoid
            // an llvm-cov artifact where a closing `}` before `continue` is
            // counted as an unreachable region.
            let ec = self.stack[depth - 1].external_cursor;
            let pushed = ec <= 15 && {
                let above = node.external >> ec;
                if above == 0 {
                    self.stack[depth - 1].external_cursor = 16;
                    false
                } else {
                    let nib = ec + above.trailing_zeros();
                    self.stack[depth - 1].external_cursor = nib + 1;
                    let child_addr =
                        addr | ((nib as u128) << (self.addr_bits - (hop + 1) * STRIDE));
                    let child_idx = rank(node.external, nib);
                    // Borrow through the copied &'a TbNode<V>, not through self.stack.
                    let child_node: &'a TbNode<V> = &node.children[child_idx];
                    self.stack.push(IterFrame {
                        node: child_node,
                        hop: hop + 1,
                        addr: child_addr,
                        internal_cursor: 1,
                        external_cursor: 0,
                    });
                    true
                }
            };

            // Both exhausted — backtrack.
            if !pushed {
                self.stack.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ── rank primitive ────────────────────────────────────────────────────────

    #[test]
    fn rank_empty_bitmap() {
        assert_eq!(rank(0b000000, 3), 0);
    }

    #[test]
    fn rank_all_set_below() {
        // bits 0,1,2 set; position 3 → 3 bits below
        assert_eq!(rank(0b00000111, 3), 3);
    }

    #[test]
    fn rank_bit_at_position_does_not_count() {
        // bit 3 is set but we ask for rank AT position 3 (bits strictly below)
        assert_eq!(rank(0b00001000, 3), 0);
    }

    #[test]
    fn rank_mixed() {
        // bits 1 and 3 set; asking for rank at position 6 → 2 bits below position 6
        assert_eq!(rank(0b00001010, 6), 2);
    }

    #[test]
    fn rank_example_from_reference() {
        // internal = bits set at positions 1, 3, 6; rank at position 6 → 2
        let bitmap = (1 << 1) | (1 << 3) | (1 << 6);
        assert_eq!(rank(bitmap, 6), 2);
    }

    // ── Empty table ───────────────────────────────────────────────────────────

    #[test]
    fn empty_table_returns_none() {
        let table: IpTable<Ipv4Addr, &str> = IpTable::new();
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    // ── Default route /0 ─────────────────────────────────────────────────────

    #[test]
    fn default_route_matches_any_address() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        assert_eq!(table.longest_match("1.2.3.4".parse().unwrap()), Some(&"default"));
        assert_eq!(table.longest_match("255.255.255.255".parse().unwrap()), Some(&"default"));
        assert_eq!(table.longest_match("0.0.0.0".parse().unwrap()), Some(&"default"));
    }

    #[test]
    fn default_route_is_fallback_when_no_specific_match() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"ten"));
        assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"default"));
    }

    // ── Single prefix ─────────────────────────────────────────────────────────

    #[test]
    fn single_prefix_hit() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"ten"));
    }

    #[test]
    fn single_prefix_miss() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn network_address_itself_matches() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("10.0.0.0".parse().unwrap()), Some(&"ten"));
    }

    #[test]
    fn address_just_outside_prefix_misses() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.0".parse().unwrap()), None);
    }

    #[test]
    fn last_address_in_prefix_matches() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/24".parse().unwrap(), "subnet");
        assert_eq!(table.longest_match("10.0.0.255".parse().unwrap()), Some(&"subnet"));
    }

    // ── Most specific wins ────────────────────────────────────────────────────

    #[test]
    fn most_specific_prefix_wins() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"specific"));
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"broad"));
    }

    #[test]
    fn three_levels_of_nesting() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "level-1");
        table.insert("10.20.0.0/16".parse().unwrap(), "level-2");
        table.insert("10.20.30.0/24".parse().unwrap(), "level-3");
        assert_eq!(table.longest_match("10.20.30.1".parse().unwrap()), Some(&"level-3"));
        assert_eq!(table.longest_match("10.20.99.1".parse().unwrap()), Some(&"level-2"));
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"level-1"));
        assert_eq!(table.longest_match("9.0.0.1".parse().unwrap()), None);
    }

    // ── /32 exact host match ──────────────────────────────────────────────────

    #[test]
    fn slash32_matches_only_that_host() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.0.0.1/32".parse().unwrap(), "host");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"host"));
        assert_eq!(table.longest_match("10.0.0.2".parse().unwrap()), Some(&"broad"));
    }

    // ── Overwrite ─────────────────────────────────────────────────────────────

    #[test]
    fn inserting_same_prefix_twice_overwrites_value() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"second"));
    }

    // ── Non-overlapping prefixes ──────────────────────────────────────────────

    #[test]
    fn non_overlapping_prefixes_do_not_cross_match() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.insert("192.168.0.0/16".parse().unwrap(), "office");
        assert_eq!(table.longest_match("10.1.2.3".parse().unwrap()), Some(&"ten"));
        assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"office"));
        assert_eq!(table.longest_match("172.16.0.1".parse().unwrap()), None);
    }

    // ── IPv6 ──────────────────────────────────────────────────────────────────

    #[test]
    fn ipv6_basic_match() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
        table.insert("2001:db8::/32".parse().unwrap(), "docs");
        assert_eq!(table.longest_match("2001:db8::1".parse().unwrap()), Some(&"docs"));
        assert_eq!(table.longest_match("2001:db9::1".parse().unwrap()), None);
    }

    #[test]
    fn ipv6_most_specific_wins() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
        table.insert("2001:db8::/32".parse().unwrap(), "broad");
        table.insert("2001:db8:1::/48".parse().unwrap(), "specific");
        assert_eq!(table.longest_match("2001:db8:1::1".parse().unwrap()), Some(&"specific"));
        assert_eq!(table.longest_match("2001:db8:2::1".parse().unwrap()), Some(&"broad"));
    }

    #[test]
    fn ipv6_default_route() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
        table.insert("::/0".parse().unwrap(), "default");
        assert_eq!(table.longest_match("2001:db8::1".parse().unwrap()), Some(&"default"));
        assert_eq!(table.longest_match("::1".parse().unwrap()), Some(&"default"));
    }

    // ── remove ────────────────────────────────────────────────────────────────

    #[test]
    fn remove_returns_the_value() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.0.0.0/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_makes_prefix_unmatchable() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_nonexistent_prefix_returns_none() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("192.168.0.0/16".parse().unwrap()), None);
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"ten"));
    }

    #[test]
    fn remove_specific_falls_back_to_general() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"broad"));
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"broad"));
    }

    #[test]
    fn remove_general_keeps_specific() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"specific"));
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_with_unmasked_prefix_finds_entry() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.99.99.99/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_default_route() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("0.0.0.0/0".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"ten"));
        assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_all_prefixes_empties_table() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.insert("10.20.0.0/16".parse().unwrap(), "b");
        table.remove("10.0.0.0/8".parse().unwrap());
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), None);
    }

    // ── contains ──────────────────────────────────────────────────────────────

    #[test]
    fn contains_inserted_prefix() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_uninserted_prefix() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("192.168.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_more_specific_prefix_that_was_not_inserted() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_broader_prefix_that_was_not_inserted() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_false_after_remove() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_with_unmasked_prefix() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.99.99.99/8".parse().unwrap()));
    }

    #[test]
    fn empty_table_contains_nothing() {
        let table: IpTable<Ipv4Addr, &str> = IpTable::new();
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    // ── non-stride-aligned prefix lengths ─────────────────────────────────────
    // These exercise rel_len 1, 2, 3 (the internal bitmap rows beyond row 0).

    #[test]
    fn prefix_length_not_multiple_of_stride() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        // /10 = 2 full strides + rel_len 2
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");
        assert_eq!(table.longest_match("10.64.0.1".parse().unwrap()), Some(&"slash10"));
        assert_eq!(table.longest_match("10.128.0.1".parse().unwrap()), Some(&"broad"));
    }

    #[test]
    fn prefix_length_one() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        // /1 covers 0.0.0.0–127.255.255.255
        table.insert("0.0.0.0/1".parse().unwrap(), "low-half");
        assert_eq!(table.longest_match("1.2.3.4".parse().unwrap()), Some(&"low-half"));
        assert_eq!(table.longest_match("128.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn prefix_length_two() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("128.0.0.0/2".parse().unwrap(), "class-b");
        assert_eq!(table.longest_match("191.255.255.255".parse().unwrap()), Some(&"class-b"));
        assert_eq!(table.longest_match("64.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn prefix_length_three() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/3".parse().unwrap(), "slash3");
        // 10.0.0.0/3: binary 00001010 → top 3 bits are 000 → covers 0.0.0.0–31.255.255.255
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"slash3"));
        assert_eq!(table.longest_match("31.255.255.255".parse().unwrap()), Some(&"slash3"));
        assert_eq!(table.longest_match("32.0.0.1".parse().unwrap()), None);
    }

    // ── remove / contains with non-stride-aligned lengths ─────────────────────
    // The rel_len > 0 branches in remove_at (line 303) and contains_at (line 350)
    // are only reachable when prefix length % STRIDE != 0.  The early-return in
    // remove_at when the destination bit is unset (line 310) requires navigating
    // to the correct node and finding nothing there.

    #[test]
    fn remove_non_stride_aligned_prefix() {
        // /10 = 2 full hops + rel_len 2 — exercises remove_at line 303
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        assert_eq!(table.remove("10.64.0.0/10".parse().unwrap()), Some("slash10"));
        // broad prefix is unaffected
        assert_eq!(table.longest_match("10.64.0.1".parse().unwrap()), Some(&"broad"));
        assert!(!table.contains("10.64.0.0/10".parse().unwrap()));
    }

    #[test]
    fn remove_non_stride_aligned_prefix_not_present() {
        // Navigates to the correct destination node but the internal bit is unset —
        // exercises remove_at line 310.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");

        // /10 was never inserted; the node it would live in exists (created for
        // /8), but the rel_len=2 internal bit for 10.64.0.0 is not set.
        assert_eq!(table.remove("10.64.0.0/10".parse().unwrap()), None);
        // table is unchanged
        assert_eq!(table.longest_match("10.64.0.1".parse().unwrap()), Some(&"broad"));
    }

    #[test]
    fn contains_non_stride_aligned_prefix() {
        // Exercises contains_at line 350 (rel_len > 0 branch).
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        assert!(table.contains("10.64.0.0/10".parse().unwrap()));
        assert!(!table.contains("10.0.0.0/10".parse().unwrap())); // different /10 block
    }

    // ── iter ──────────────────────────────────────────────────────────────────

    fn sorted_entries<A: IpAddress, V: Clone>(
        table: &IpTable<A, V>,
    ) -> Vec<(String, V)> {
        let mut entries: Vec<_> = table
            .iter()
            .map(|(p, v)| (format!("{}/{}", p.ip(), p.mask()), v.clone()))
            .collect();
        entries.sort_by_key(|(p, _)| p.clone());
        entries
    }

    #[test]
    fn iter_empty_table() {
        let table: IpTable<Ipv4Addr, &str> = IpTable::new();
        assert_eq!(table.iter().count(), 0);
    }

    #[test]
    fn iter_single_default_route() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("0.0.0.0/0".to_string(), "default")]);
    }

    #[test]
    fn iter_single_host_route() {
        // /32 is stored at maximum depth — exercises the post-loop node visit.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.1/32".parse().unwrap(), "host");
        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("10.0.0.1/32".to_string(), "host")]);
    }

    #[test]
    fn iter_multiple_prefixes_all_present() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("0.0.0.0/0".parse().unwrap(),    "default");
        table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        table.insert("10.20.30.0/24".parse().unwrap(), "narrow");

        let entries = sorted_entries(&table);
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().any(|(_, v)| *v == "default"));
        assert!(entries.iter().any(|(_, v)| *v == "broad"));
        assert!(entries.iter().any(|(_, v)| *v == "specific"));
        assert!(entries.iter().any(|(_, v)| *v == "narrow"));
    }

    #[test]
    fn iter_reconstructs_prefix_correctly() {
        // Verifies that the address and mask are faithfully recovered.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("192.168.1.0/24".parse().unwrap(), "subnet");

        let (prefix, value) = table.iter().next().unwrap();
        assert_eq!(format!("{}", prefix.ip()), "192.168.1.0");
        assert_eq!(prefix.mask(), 24);
        assert_eq!(*value, "subnet");
    }

    #[test]
    fn iter_non_stride_aligned_prefix() {
        // /10 has rel_len=2; checks that internal bitmap decoding is correct.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("10.64.0.0/10".to_string(), "slash10")]);
    }

    #[test]
    fn iter_count_matches_insert_count() {
        // Round-trip: insert N distinct prefixes, iter must yield exactly N.
        let mut table: IpTable<Ipv4Addr, u32> = IpTable::new();
        let prefixes = [
            "0.0.0.0/0", "10.0.0.0/8", "10.0.0.0/16", "10.0.0.0/24",
            "172.16.0.0/12", "192.168.0.0/16", "192.168.1.0/24", "10.0.0.1/32",
        ];
        for (i, p) in prefixes.iter().enumerate() {
            table.insert(p.parse().unwrap(), i as u32);
        }
        assert_eq!(table.iter().count(), prefixes.len());
    }

    #[test]
    fn iter_ipv6() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
        table.insert("::/0".parse().unwrap(),          "default");
        table.insert("2001:db8::/32".parse().unwrap(), "docs");
        table.insert("2001:db8:1::/48".parse().unwrap(), "subnet");

        let entries = sorted_entries(&table);
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|(_, v)| *v == "default"));
        assert!(entries.iter().any(|(_, v)| *v == "docs"));
        assert!(entries.iter().any(|(_, v)| *v == "subnet"));
    }

    #[test]
    fn iter_multiple_external_children_exhausted() {
        // Forces the external-cursor-exhausted path (ec <= 15, above == 0)
        // by giving the root node three external children. After iterating
        // through all three subtrees the cursor scans past the last set bit.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        // nibble[0] of each: 0x0A=10→nib 0, 0x40=64→nib 4, 0xC0=192→nib 12
        table.insert("10.0.0.0/8".parse().unwrap(),    "ten");
        table.insert("64.0.0.0/8".parse().unwrap(),    "sixty-four");
        table.insert("192.168.0.0/16".parse().unwrap(), "office");

        assert_eq!(table.iter().count(), 3);
    }

    #[test]
    fn iter_after_remove_reflects_change() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(),   "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");

        table.remove("10.0.0.0/8".parse().unwrap());

        let entries = sorted_entries(&table);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ("10.20.0.0/16".to_string(), "specific"));
    }
}
