use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::fmt;
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
/// use routemap::RouteMap;
/// use std::net::Ipv4Addr;
///
/// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
/// table.insert("10.0.0.0/8".parse().unwrap(), "datacenter");
/// table.insert("10.20.0.0/16".parse().unwrap(), "third-floor");
///
/// assert_eq!(
///     table.longest_match("10.20.5.1".parse().unwrap()),
///     Some(&"third-floor")
/// );
/// assert_eq!(
///     table.longest_match("10.99.0.1".parse().unwrap()),
///     Some(&"datacenter")
/// );
/// assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
/// ```
pub struct RouteMap<A: IpAddress, V> {
    root: TbNode<V>,
    count: usize,
    _marker: PhantomData<A>,
}

impl<A: IpAddress, V> RouteMap<A, V> {
    /// Creates a new, empty LPM table.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// ```
    pub fn new() -> Self {
        Self {
            root: TbNode::new(),
            count: 0,
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
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
    /// table.insert("10.0.0.0/8".parse().unwrap(), "updated");
    /// assert_eq!(
    ///     table.longest_match("10.0.0.1".parse().unwrap()),
    ///     Some(&"updated")
    /// );
    /// ```
    pub fn insert(&mut self, prefix: IpPrefix<A>, value: V) {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        let is_new = insert_at(
            &mut self.root,
            addr,
            A::BITS as u32,
            0,
            len / STRIDE,
            len % STRIDE,
            value,
        );
        if is_new {
            self.count += 1;
        }
    }

    /// Returns a reference to the value for the most specific matching prefix,
    /// or `None` if no prefix in the table matches `addr`.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// table.insert("0.0.0.0/0".parse().unwrap(), "default");
    /// table.insert("10.0.0.0/8".parse().unwrap(), "datacenter");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "third-floor");
    ///
    /// assert_eq!(
    ///     table.longest_match("10.20.5.1".parse().unwrap()),
    ///     Some(&"third-floor")
    /// );
    /// assert_eq!(
    ///     table.longest_match("10.99.0.1".parse().unwrap()),
    ///     Some(&"datacenter")
    /// );
    /// assert_eq!(
    ///     table.longest_match("192.168.1.1".parse().unwrap()),
    ///     Some(&"default")
    /// );
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
                let rel_v = if rel_len == 0 {
                    0u32
                } else {
                    nib >> (STRIDE - rel_len)
                };
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
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "specific");
    ///
    /// assert_eq!(
    ///     table.remove("10.20.0.0/16".parse().unwrap()),
    ///     Some("specific")
    /// );
    /// assert_eq!(
    ///     table.longest_match("10.20.5.1".parse().unwrap()),
    ///     Some(&"broad")
    /// );
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
        if value.is_some() {
            self.count -= 1;
        }
        value
    }

    /// Returns a reference to the value for an exact prefix match, or `None`
    /// if this prefix is not in the table.
    ///
    /// Unlike [`longest_match`](Self::longest_match), this never falls back to
    /// a covering prefix — the prefix must be present exactly. Host bits in the
    /// prefix address are ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "specific");
    ///
    /// assert_eq!(table.get("10.0.0.0/8".parse().unwrap()), Some(&"broad"));
    /// assert_eq!(
    ///     table.get("10.20.0.0/16".parse().unwrap()),
    ///     Some(&"specific")
    /// );
    /// assert_eq!(table.get("10.99.0.0/16".parse().unwrap()), None);
    /// ```
    pub fn get(&self, prefix: IpPrefix<A>) -> Option<&V> {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        get_at(
            &self.root,
            addr,
            A::BITS as u32,
            0,
            len / STRIDE,
            len % STRIDE,
        )
    }

    /// Returns `true` if the table contains an exact entry for this prefix.
    ///
    /// This is an exact match — not a longest-prefix match. Host bits are ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
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
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// table.insert("10.0.0.0/8".parse().unwrap(), "broad");
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

    /// Returns `true` if the table contains an exact entry for this prefix.
    ///
    /// This is an exact match — not a longest-prefix match. Host bits are ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
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

    /// Returns the number of prefix entries in the table.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// assert_eq!(table.len(), 0);
    /// table.insert("10.0.0.0/8".parse().unwrap(), "a");
    /// table.insert("10.20.0.0/16".parse().unwrap(), "b");
    /// assert_eq!(table.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.count
    }

    /// Returns `true` if the table contains no entries.
    ///
    /// # Example
    ///
    /// ```
    /// use routemap::RouteMap;
    /// use std::net::Ipv4Addr;
    ///
    /// let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
    /// assert!(table.is_empty());
    /// table.insert("10.0.0.0/8".parse().unwrap(), "ten");
    /// assert!(!table.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl<A: IpAddress, V> Default for RouteMap<A, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: IpAddress + fmt::Display, V: fmt::Debug> fmt::Debug for RouteMap<A, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (prefix, value) in self.iter() {
            let key = format!("{}/{}", prefix.ip(), prefix.mask());
            map.entry(&key, value);
        }
        map.finish()
    }
}

impl<A: IpAddress, V> FromIterator<(IpPrefix<A>, V)> for RouteMap<A, V> {
    fn from_iter<I: IntoIterator<Item = (IpPrefix<A>, V)>>(iter: I) -> Self {
        let mut table = Self::new();
        for (prefix, value) in iter {
            table.insert(prefix, value);
        }
        table
    }
}

impl<'a, A: IpAddress, V> IntoIterator for &'a RouteMap<A, V> {
    type Item = (IpPrefix<A>, &'a V);
    type IntoIter = Iter<'a, A, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

// Returns true when a new entry was created, false when an existing value was overwritten.
fn insert_at<V>(
    node: &mut TbNode<V>,
    addr: u128,
    addr_bits: u32,
    current_hop: u32,
    full_hops: u32,
    rel_len: u32,
    value: V,
) -> bool {
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
            false // overwrite
        } else {
            node.values.insert(idx, value);
            node.internal |= 1 << bpos;
            true // new entry
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
        )
    }
}

fn get_at<V>(
    node: &TbNode<V>,
    addr: u128,
    addr_bits: u32,
    current_hop: u32,
    full_hops: u32,
    rel_len: u32,
) -> Option<&V> {
    if current_hop == full_hops {
        let nib = if rel_len > 0 {
            nibble(addr, addr_bits, current_hop) >> (STRIDE - rel_len)
        } else {
            0
        };
        let bpos = (1u32 << rel_len) + nib;
        if (node.internal >> bpos) & 1 == 1 {
            Some(&node.values[rank(node.internal, bpos)])
        } else {
            None
        }
    } else {
        let nib = nibble(addr, addr_bits, current_hop);
        if (node.external >> nib) & 1 == 0 {
            return None;
        }
        let child_idx = rank(node.external, nib);
        get_at(
            &node.children[child_idx],
            addr,
            addr_bits,
            current_hop + 1,
            full_hops,
            rel_len,
        )
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
        let (value, prune) = remove_at(
            &mut node.children[child_idx],
            addr,
            addr_bits,
            current_hop + 1,
            full_hops,
            rel_len,
        );

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

/// An iterator over all `(prefix, &value)` pairs in an [`RouteMap`].
///
/// Entries are yielded in depth-first order: a node's internal prefixes
/// (shorter to longer within the stride) before its children's prefixes.
///
/// Created by [`RouteMap::iter`].
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
                        addr | ((rel_bits as u128) << (self.addr_bits - hop * STRIDE - rel_len))
                    } else {
                        addr
                    };

                    let idx = rank(node.internal, bpos);
                    let value: &'a V = &node.values[idx];
                    let prefix = IpPrefix::new(A::from_u128(full_addr), full_len as u8).unwrap();
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
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    // ── Default route /0 ─────────────────────────────────────────────────────

    #[test]
    fn default_route_matches_any_address() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        assert_eq!(
            table.longest_match("1.2.3.4".parse().unwrap()),
            Some(&"default")
        );
        assert_eq!(
            table.longest_match("255.255.255.255".parse().unwrap()),
            Some(&"default")
        );
        assert_eq!(
            table.longest_match("0.0.0.0".parse().unwrap()),
            Some(&"default")
        );
    }

    #[test]
    fn default_route_is_fallback_when_no_specific_match() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
        assert_eq!(
            table.longest_match("192.168.1.1".parse().unwrap()),
            Some(&"default")
        );
    }

    // ── Single prefix ─────────────────────────────────────────────────────────

    #[test]
    fn single_prefix_hit() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn single_prefix_miss() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn network_address_itself_matches() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.0".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn address_just_outside_prefix_misses() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.0".parse().unwrap()), None);
    }

    #[test]
    fn last_address_in_prefix_matches() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/24".parse().unwrap(), "subnet");
        assert_eq!(
            table.longest_match("10.0.0.255".parse().unwrap()),
            Some(&"subnet")
        );
    }

    // ── Most specific wins ────────────────────────────────────────────────────

    #[test]
    fn most_specific_prefix_wins() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        assert_eq!(
            table.longest_match("10.20.5.1".parse().unwrap()),
            Some(&"specific")
        );
        assert_eq!(
            table.longest_match("10.99.0.1".parse().unwrap()),
            Some(&"broad")
        );
    }

    #[test]
    fn three_levels_of_nesting() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "level-1");
        table.insert("10.20.0.0/16".parse().unwrap(), "level-2");
        table.insert("10.20.30.0/24".parse().unwrap(), "level-3");
        assert_eq!(
            table.longest_match("10.20.30.1".parse().unwrap()),
            Some(&"level-3")
        );
        assert_eq!(
            table.longest_match("10.20.99.1".parse().unwrap()),
            Some(&"level-2")
        );
        assert_eq!(
            table.longest_match("10.99.0.1".parse().unwrap()),
            Some(&"level-1")
        );
        assert_eq!(table.longest_match("9.0.0.1".parse().unwrap()), None);
    }

    // ── /32 exact host match ──────────────────────────────────────────────────

    #[test]
    fn slash32_matches_only_that_host() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.0.0.1/32".parse().unwrap(), "host");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"host")
        );
        assert_eq!(
            table.longest_match("10.0.0.2".parse().unwrap()),
            Some(&"broad")
        );
    }

    // ── Overwrite ─────────────────────────────────────────────────────────────

    #[test]
    fn inserting_same_prefix_twice_overwrites_value() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"second")
        );
    }

    // ── Non-overlapping prefixes ──────────────────────────────────────────────

    #[test]
    fn non_overlapping_prefixes_do_not_cross_match() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.insert("192.168.0.0/16".parse().unwrap(), "office");
        assert_eq!(
            table.longest_match("10.1.2.3".parse().unwrap()),
            Some(&"ten")
        );
        assert_eq!(
            table.longest_match("192.168.1.1".parse().unwrap()),
            Some(&"office")
        );
        assert_eq!(table.longest_match("172.16.0.1".parse().unwrap()), None);
    }

    // ── IPv6 ──────────────────────────────────────────────────────────────────

    #[test]
    fn ipv6_basic_match() {
        let mut table: RouteMap<Ipv6Addr, &str> = RouteMap::new();
        table.insert("2001:db8::/32".parse().unwrap(), "docs");
        assert_eq!(
            table.longest_match("2001:db8::1".parse().unwrap()),
            Some(&"docs")
        );
        assert_eq!(table.longest_match("2001:db9::1".parse().unwrap()), None);
    }

    #[test]
    fn ipv6_most_specific_wins() {
        let mut table: RouteMap<Ipv6Addr, &str> = RouteMap::new();
        table.insert("2001:db8::/32".parse().unwrap(), "broad");
        table.insert("2001:db8:1::/48".parse().unwrap(), "specific");
        assert_eq!(
            table.longest_match("2001:db8:1::1".parse().unwrap()),
            Some(&"specific")
        );
        assert_eq!(
            table.longest_match("2001:db8:2::1".parse().unwrap()),
            Some(&"broad")
        );
    }

    #[test]
    fn ipv6_default_route() {
        let mut table: RouteMap<Ipv6Addr, &str> = RouteMap::new();
        table.insert("::/0".parse().unwrap(), "default");
        assert_eq!(
            table.longest_match("2001:db8::1".parse().unwrap()),
            Some(&"default")
        );
        assert_eq!(
            table.longest_match("::1".parse().unwrap()),
            Some(&"default")
        );
    }

    // ── remove ────────────────────────────────────────────────────────────────

    #[test]
    fn remove_returns_the_value() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.0.0.0/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_makes_prefix_unmatchable() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_nonexistent_prefix_returns_none() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("192.168.0.0/16".parse().unwrap()), None);
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn remove_specific_falls_back_to_general() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(
            table.longest_match("10.20.5.1".parse().unwrap()),
            Some(&"broad")
        );
        assert_eq!(
            table.longest_match("10.99.0.1".parse().unwrap()),
            Some(&"broad")
        );
    }

    #[test]
    fn remove_general_keeps_specific() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(
            table.longest_match("10.20.5.1".parse().unwrap()),
            Some(&"specific")
        );
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_with_unmasked_prefix_finds_entry() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.99.99.99/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_default_route() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("0.0.0.0/0".parse().unwrap());
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
        assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_all_prefixes_empties_table() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.insert("10.20.0.0/16".parse().unwrap(), "b");
        table.remove("10.0.0.0/8".parse().unwrap());
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), None);
    }

    // ── get ───────────────────────────────────────────────────────────────────

    #[test]
    fn get_returns_exact_entry() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");

        assert_eq!(table.get("10.0.0.0/8".parse().unwrap()), Some(&"broad"));
        assert_eq!(
            table.get("10.20.0.0/16".parse().unwrap()),
            Some(&"specific")
        );
    }

    #[test]
    fn get_returns_none_for_covering_prefix() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");

        // /16 was never inserted — get must not fall back to /8
        assert_eq!(table.get("10.20.0.0/16".parse().unwrap()), None);
    }

    #[test]
    fn get_returns_none_for_missing_prefix() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert_eq!(table.get("10.0.0.0/8".parse().unwrap()), None);
    }

    #[test]
    fn get_with_unmasked_prefix_finds_entry() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.get("10.99.99.99/8".parse().unwrap()), Some(&"ten"));
    }

    #[test]
    fn get_returns_none_after_remove() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.get("10.0.0.0/8".parse().unwrap()), None);
    }

    #[test]
    fn get_non_stride_aligned_prefix() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        assert_eq!(table.get("10.64.0.0/10".parse().unwrap()), Some(&"slash10"));
        assert_eq!(table.get("10.0.0.0/10".parse().unwrap()), None);
    }

    // ── contains ──────────────────────────────────────────────────────────────

    #[test]
    fn contains_inserted_prefix() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_uninserted_prefix() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("192.168.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_more_specific_prefix_that_was_not_inserted() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_broader_prefix_that_was_not_inserted() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_false_after_remove() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_with_unmasked_prefix() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.99.99.99/8".parse().unwrap()));
    }

    #[test]
    fn empty_table_contains_nothing() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    // ── non-stride-aligned prefix lengths ─────────────────────────────────────
    // These exercise rel_len 1, 2, 3 (the internal bitmap rows beyond row 0).

    #[test]
    fn prefix_length_not_multiple_of_stride() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        // /10 = 2 full strides + rel_len 2
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");
        assert_eq!(
            table.longest_match("10.64.0.1".parse().unwrap()),
            Some(&"slash10")
        );
        assert_eq!(
            table.longest_match("10.128.0.1".parse().unwrap()),
            Some(&"broad")
        );
    }

    #[test]
    fn prefix_length_one() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        // /1 covers 0.0.0.0–127.255.255.255
        table.insert("0.0.0.0/1".parse().unwrap(), "low-half");
        assert_eq!(
            table.longest_match("1.2.3.4".parse().unwrap()),
            Some(&"low-half")
        );
        assert_eq!(table.longest_match("128.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn prefix_length_two() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("128.0.0.0/2".parse().unwrap(), "class-b");
        assert_eq!(
            table.longest_match("191.255.255.255".parse().unwrap()),
            Some(&"class-b")
        );
        assert_eq!(table.longest_match("64.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn prefix_length_three() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/3".parse().unwrap(), "slash3");
        // 10.0.0.0/3: binary 00001010 → top 3 bits are 000 → covers 0.0.0.0–31.255.255.255
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"slash3")
        );
        assert_eq!(
            table.longest_match("31.255.255.255".parse().unwrap()),
            Some(&"slash3")
        );
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
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        assert_eq!(
            table.remove("10.64.0.0/10".parse().unwrap()),
            Some("slash10")
        );
        // broad prefix is unaffected
        assert_eq!(
            table.longest_match("10.64.0.1".parse().unwrap()),
            Some(&"broad")
        );
        assert!(!table.contains("10.64.0.0/10".parse().unwrap()));
    }

    #[test]
    fn remove_non_stride_aligned_prefix_not_present() {
        // Navigates to the correct destination node but the internal bit is unset —
        // exercises remove_at line 310.
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");

        // /10 was never inserted; the node it would live in exists (created for
        // /8), but the rel_len=2 internal bit for 10.64.0.0 is not set.
        assert_eq!(table.remove("10.64.0.0/10".parse().unwrap()), None);
        // table is unchanged
        assert_eq!(
            table.longest_match("10.64.0.1".parse().unwrap()),
            Some(&"broad")
        );
    }

    #[test]
    fn contains_non_stride_aligned_prefix() {
        // Exercises contains_at line 350 (rel_len > 0 branch).
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        assert!(table.contains("10.64.0.0/10".parse().unwrap()));
        assert!(!table.contains("10.0.0.0/10".parse().unwrap())); // different /10 block
    }

    // ── iter ──────────────────────────────────────────────────────────────────

    fn sorted_entries<A: IpAddress, V: Clone>(table: &RouteMap<A, V>) -> Vec<(String, V)> {
        let mut entries: Vec<_> = table
            .iter()
            .map(|(p, v)| (format!("{}/{}", p.ip(), p.mask()), v.clone()))
            .collect();
        entries.sort_by_key(|(p, _)| p.clone());
        entries
    }

    #[test]
    fn iter_empty_table() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert_eq!(table.iter().count(), 0);
    }

    #[test]
    fn iter_single_default_route() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("0.0.0.0/0".to_string(), "default")]);
    }

    #[test]
    fn iter_single_host_route() {
        // /32 is stored at maximum depth — exercises the post-loop node visit.
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.1/32".parse().unwrap(), "host");
        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("10.0.0.1/32".to_string(), "host")]);
    }

    #[test]
    fn iter_multiple_prefixes_all_present() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("0.0.0.0/0".parse().unwrap(), "default");
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
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
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("192.168.1.0/24".parse().unwrap(), "subnet");

        let (prefix, value) = table.iter().next().unwrap();
        assert_eq!(format!("{}", prefix.ip()), "192.168.1.0");
        assert_eq!(prefix.mask(), 24);
        assert_eq!(*value, "subnet");
    }

    #[test]
    fn iter_non_stride_aligned_prefix() {
        // /10 has rel_len=2; checks that internal bitmap decoding is correct.
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.64.0.0/10".parse().unwrap(), "slash10");

        let entries = sorted_entries(&table);
        assert_eq!(entries, vec![("10.64.0.0/10".to_string(), "slash10")]);
    }

    #[test]
    fn iter_count_matches_insert_count() {
        // Round-trip: insert N distinct prefixes, iter must yield exactly N.
        let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
        let prefixes = [
            "0.0.0.0/0",
            "10.0.0.0/8",
            "10.0.0.0/16",
            "10.0.0.0/24",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "192.168.1.0/24",
            "10.0.0.1/32",
        ];
        for (i, p) in prefixes.iter().enumerate() {
            table.insert(p.parse().unwrap(), i as u32);
        }
        assert_eq!(table.iter().count(), prefixes.len());
    }

    #[test]
    fn iter_ipv6() {
        let mut table: RouteMap<Ipv6Addr, &str> = RouteMap::new();
        table.insert("::/0".parse().unwrap(), "default");
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
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        // nibble[0] of each: 0x0A=10→nib 0, 0x40=64→nib 4, 0xC0=192→nib 12
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.insert("64.0.0.0/8".parse().unwrap(), "sixty-four");
        table.insert("192.168.0.0/16".parse().unwrap(), "office");

        assert_eq!(table.iter().count(), 3);
    }

    #[test]
    fn iter_after_remove_reflects_change() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");

        table.remove("10.0.0.0/8".parse().unwrap());

        let entries = sorted_entries(&table);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ("10.20.0.0/16".to_string(), "specific"));
    }

    // ── Default ───────────────────────────────────────────────────────────────

    #[test]
    fn default_produces_empty_table() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::default();
        assert_eq!(table.iter().count(), 0);
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    // ── Debug ─────────────────────────────────────────────────────────────────

    #[test]
    fn debug_empty_table() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert_eq!(format!("{:?}", table), "{}");
    }

    #[test]
    fn debug_contains_prefix_and_value() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "datacenter");
        let output = format!("{:?}", table);
        assert!(output.contains("10.0.0.0/8"), "output was: {output}");
        assert!(output.contains("datacenter"), "output was: {output}");
    }

    #[test]
    fn debug_ipv6() {
        let mut table: RouteMap<Ipv6Addr, u32> = RouteMap::new();
        table.insert("2001:db8::/32".parse().unwrap(), 42);
        let output = format!("{:?}", table);
        assert!(output.contains("2001:db8::/32"), "output was: {output}");
        assert!(output.contains("42"), "output was: {output}");
    }

    // ── FromIterator ──────────────────────────────────────────────────────────

    #[test]
    fn collect_from_iterator() {
        use ipnetx::prefix::IpPrefix;
        let pairs: Vec<(IpPrefix<Ipv4Addr>, &str)> = vec![
            ("10.0.0.0/8".parse().unwrap(), "broad"),
            ("10.20.0.0/16".parse().unwrap(), "specific"),
        ];
        let table: RouteMap<Ipv4Addr, &str> = pairs.into_iter().collect();
        assert_eq!(
            table.longest_match("10.20.5.1".parse().unwrap()),
            Some(&"specific")
        );
        assert_eq!(
            table.longest_match("10.99.0.1".parse().unwrap()),
            Some(&"broad")
        );
        assert_eq!(table.iter().count(), 2);
    }

    #[test]
    fn iter_collect_round_trip() {
        let mut original: RouteMap<Ipv4Addr, u32> = RouteMap::new();
        original.insert("0.0.0.0/0".parse().unwrap(), 0);
        original.insert("10.0.0.0/8".parse().unwrap(), 1);
        original.insert("10.20.0.0/16".parse().unwrap(), 2);
        original.insert("192.168.0.0/16".parse().unwrap(), 3);

        let restored: RouteMap<Ipv4Addr, u32> = original.iter().map(|(p, &v)| (p, v)).collect();

        assert_eq!(restored.iter().count(), 4);
        assert_eq!(
            restored.longest_match("10.20.5.1".parse().unwrap()),
            Some(&2)
        );
        assert_eq!(
            restored.longest_match("10.99.0.1".parse().unwrap()),
            Some(&1)
        );
        assert_eq!(
            restored.longest_match("192.168.1.1".parse().unwrap()),
            Some(&3)
        );
        assert_eq!(restored.longest_match("8.8.8.8".parse().unwrap()), Some(&0));
    }

    // ── len / is_empty ────────────────────────────────────────────────────────

    #[test]
    fn len_empty_table() {
        let table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn len_tracks_inserts() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
        table.insert("10.20.0.0/16".parse().unwrap(), "b");
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn len_overwrite_does_not_increment() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn len_tracks_removes() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.insert("10.20.0.0/16".parse().unwrap(), "b");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.len(), 1);
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn len_remove_nonexistent_does_not_decrement() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.remove("192.168.0.0/16".parse().unwrap());
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn len_matches_iter_count() {
        let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
        let prefixes = ["0.0.0.0/0", "10.0.0.0/8", "10.20.0.0/16", "192.168.1.0/24"];
        for (i, p) in prefixes.iter().enumerate() {
            table.insert(p.parse().unwrap(), i as u32);
        }
        assert_eq!(table.len(), table.iter().count());
    }

    // ── IntoIterator ──────────────────────────────────────────────────────────

    #[test]
    fn into_iter_for_loop_syntax() {
        let mut table: RouteMap<Ipv4Addr, &str> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.insert("10.20.0.0/16".parse().unwrap(), "twenty");

        let mut count = 0;
        for (_prefix, _value) in &table {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn into_iter_collect() {
        let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
        table.insert("10.0.0.0/8".parse().unwrap(), 1);
        table.insert("192.168.0.0/16".parse().unwrap(), 2);

        let entries: Vec<_> = (&table).into_iter().collect();
        assert_eq!(entries.len(), 2);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn v4(addr: u32, len: u8) -> IpPrefix<Ipv4Addr> {
        IpPrefix::new(Ipv4Addr::from(addr), len).unwrap()
    }

    fn v6(addr: u128, len: u8) -> IpPrefix<Ipv6Addr> {
        IpPrefix::new(Ipv6Addr::from(addr), len).unwrap()
    }

    // Returns the network mask for a given IPv4 prefix length.
    fn mask4(len: u8) -> u32 {
        if len == 0 {
            0
        } else {
            !0u32 << (32 - len as u32)
        }
    }

    // Returns the network mask for a given IPv6 prefix length.
    fn mask6(len: u8) -> u128 {
        if len == 0 {
            0
        } else {
            !0u128 << (128 - len as u32)
        }
    }

    proptest! {
        // ── Insert → contains / get roundtrip ────────────────────────────────

        // Any inserted prefix must immediately be visible via contains() and get().
        #[test]
        fn insert_implies_contains_and_get(
            addr in any::<u32>(), len in 0u8..=32u8, val in any::<u32>(),
        ) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(addr, len), val);
            prop_assert!(t.contains(v4(addr, len)));
            prop_assert_eq!(t.get(v4(addr, len)), Some(&val));
        }

        // longest_match on a prefix's own network address must return that prefix's value.
        #[test]
        fn insert_longest_match_hits_network_addr(
            addr in any::<u32>(), len in 0u8..=32u8, val in any::<u32>(),
        ) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(addr, len), val);
            let network = Ipv4Addr::from(addr & mask4(len));
            prop_assert_eq!(t.longest_match(network), Some(&val));
        }

        // ── Overwrite semantics ───────────────────────────────────────────────

        // Inserting the same prefix twice must leave len() == 1 and reflect the latest value.
        #[test]
        fn overwrite_preserves_len_and_updates_value(
            addr in any::<u32>(), len in 0u8..=32u8,
            v1 in any::<u32>(), v2 in any::<u32>(),
        ) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(addr, len), v1);
            t.insert(v4(addr, len), v2);
            prop_assert_eq!(t.len(), 1);
            prop_assert_eq!(t.get(v4(addr, len)), Some(&v2));
        }

        // ── Masked equivalence ────────────────────────────────────────────────

        // Inserting with host bits set must produce the same table as inserting the masked prefix.
        #[test]
        fn unmasked_insert_equals_masked_insert(
            addr in any::<u32>(), len in 0u8..=32u8, val in any::<u32>(),
        ) {
            let mut t1: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            let mut t2: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t1.insert(v4(addr, len), val);
            t2.insert(v4(addr & mask4(len), len), val);
            prop_assert_eq!(t1.len(), t2.len());
            for (prefix, &v) in t1.iter() {
                prop_assert_eq!(t2.get(prefix), Some(&v));
            }
        }

        // ── Remove consistency ────────────────────────────────────────────────

        // After insert + remove, the prefix must be gone from every access path.
        #[test]
        fn remove_clears_entry(
            addr in any::<u32>(), len in 0u8..=32u8, val in any::<u32>(),
        ) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(addr, len), val);
            prop_assert_eq!(t.remove(v4(addr, len)), Some(val));
            prop_assert!(!t.contains(v4(addr, len)));
            prop_assert_eq!(t.get(v4(addr, len)), None);
            prop_assert_eq!(t.len(), 0);
        }

        // ── len invariant ─────────────────────────────────────────────────────

        // len() must always equal iter().count() after any sequence of inserts.
        #[test]
        fn len_equals_iter_count(
            ops in prop::collection::vec((any::<u32>(), 0u8..=32u8, any::<u32>()), 1..=30),
        ) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            for (addr, len, val) in ops {
                t.insert(v4(addr, len), val);
            }
            prop_assert_eq!(t.len(), t.iter().count());
        }

        // ── LPM correctness ───────────────────────────────────────────────────

        // The most-specific matching prefix must win longest_match.
        #[test]
        fn more_specific_prefix_wins_lpm(
            base in any::<u32>(),
            broad_len in 0u8..=24u8,
            extra in 1u8..=8u8,
        ) {
            let specific_len = broad_len + extra;
            prop_assume!(specific_len <= 32);
            let network = Ipv4Addr::from(base & mask4(specific_len));
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(base, broad_len), 1);
            t.insert(v4(base, specific_len), 2);
            prop_assert_eq!(t.longest_match(network), Some(&2));
        }

        // After removing the specific prefix, lookups must fall back to the covering prefix.
        #[test]
        fn remove_specific_falls_back_to_broad(
            base in any::<u32>(),
            broad_len in 0u8..=24u8,
            extra in 1u8..=8u8,
        ) {
            let specific_len = broad_len + extra;
            prop_assume!(specific_len <= 32);
            let network = Ipv4Addr::from(base & mask4(specific_len));
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(base, broad_len), 1);
            t.insert(v4(base, specific_len), 2);
            t.remove(v4(base, specific_len));
            prop_assert_eq!(t.longest_match(network), Some(&1));
        }

        // /0 is a universal default: it must match every possible address.
        #[test]
        fn default_route_matches_all_addresses(lookup in any::<u32>()) {
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(0, 0), 42);
            prop_assert_eq!(t.longest_match(Ipv4Addr::from(lookup)), Some(&42));
        }

        // A /32 host route must match only its own address and nothing else.
        #[test]
        fn host_route_matches_only_exact_address(
            host in any::<u32>(), other in any::<u32>(),
        ) {
            prop_assume!(host != other);
            let mut t: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            t.insert(v4(host, 32), 99);
            prop_assert_eq!(t.longest_match(Ipv4Addr::from(host)), Some(&99));
            prop_assert_eq!(t.longest_match(Ipv4Addr::from(other)), None);
        }

        // ── Iter completeness ─────────────────────────────────────────────────

        // iter() + collect() must reconstruct a table with identical contents.
        #[test]
        fn iter_collect_roundtrip(
            ops in prop::collection::vec((any::<u32>(), 0u8..=32u8, any::<u32>()), 1..=20),
        ) {
            let mut original: RouteMap<Ipv4Addr, u32> = RouteMap::new();
            for (addr, len, val) in ops {
                original.insert(v4(addr, len), val);
            }
            let restored: RouteMap<Ipv4Addr, u32> =
                original.iter().map(|(p, &v)| (p, v)).collect();
            prop_assert_eq!(original.len(), restored.len());
            for (prefix, &val) in original.iter() {
                prop_assert_eq!(restored.get(prefix), Some(&val));
            }
        }

        // ── IPv6 parity ───────────────────────────────────────────────────────

        // IPv6: any inserted prefix must be visible via contains() and get().
        #[test]
        fn ipv6_insert_implies_contains_and_get(
            addr in any::<u128>(), len in 0u8..=128u8, val in any::<u32>(),
        ) {
            let mut t: RouteMap<Ipv6Addr, u32> = RouteMap::new();
            t.insert(v6(addr, len), val);
            prop_assert!(t.contains(v6(addr, len)));
            prop_assert_eq!(t.get(v6(addr, len)), Some(&val));
        }

        // IPv6: /0 must match every possible 128-bit address.
        #[test]
        fn ipv6_default_route_matches_all(lookup in any::<u128>()) {
            let mut t: RouteMap<Ipv6Addr, u32> = RouteMap::new();
            t.insert(v6(0, 0), 55);
            prop_assert_eq!(t.longest_match(Ipv6Addr::from(lookup)), Some(&55));
        }

        // IPv6: the more-specific prefix must win longest_match.
        #[test]
        fn ipv6_more_specific_prefix_wins_lpm(
            base in any::<u128>(),
            broad_len in 0u8..=120u8,
            extra in 1u8..=8u8,
        ) {
            let specific_len = broad_len + extra;
            prop_assume!(specific_len <= 128);
            let network = Ipv6Addr::from(base & mask6(specific_len));
            let mut t: RouteMap<Ipv6Addr, u32> = RouteMap::new();
            t.insert(v6(base, broad_len), 1);
            t.insert(v6(base, specific_len), 2);
            prop_assert_eq!(t.longest_match(network), Some(&2));
        }
    }
}
