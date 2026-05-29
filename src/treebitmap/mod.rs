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
/// treebitmap. Compared to a binary trie, each node processes 4 bits at once,
/// reducing the number of cache misses on large tables by 4×.
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
}
