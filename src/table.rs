use crate::node::TrieNode;
use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::marker::PhantomData;

/// An in-memory Longest Prefix Match (LPM) routing table.
///
/// An LPM table maps IP network prefixes — like `10.0.0.0/8` or `192.168.1.0/24` —
/// to values of any type `V`. When you look up an IP address, the table returns the
/// value associated with the *most specific* matching prefix: the one with the
/// longest prefix length.
///
/// For example, if the table contains both `10.0.0.0/8` and `10.20.0.0/16`, a
/// lookup for `10.20.5.1` returns the value for `10.20.0.0/16` because `/16` is
/// more specific than `/8`. This is the same rule every IP router on the internet
/// uses to decide where to send a packet.
///
/// # Type parameters
///
/// - `A` — the address family: [`Ipv4Addr`](std::net::Ipv4Addr) or
///   [`Ipv6Addr`](std::net::Ipv6Addr). A single table is dedicated to one address
///   family — use separate tables for IPv4 and IPv6.
/// - `V` — the value stored alongside each prefix. This can be anything: route
///   entries, ASN records, geographic metadata, firewall rules, customer identifiers.
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
/// // Most specific match wins.
/// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"third-floor"));
///
/// // Falls back to the broader prefix for other addresses in 10.x.x.x.
/// assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"datacenter"));
///
/// // No match for addresses outside all inserted prefixes.
/// assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), None);
/// ```
pub struct IpTable<A: IpAddress, V> {
    root: TrieNode<V>,
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
            root: TrieNode::new(),
            _marker: PhantomData,
        }
    }

    /// Inserts a prefix and its associated value into the table.
    ///
    /// A prefix is a network address paired with a prefix length, written in CIDR
    /// notation — for example, `10.0.0.0/8` means "all addresses whose first 8 bits
    /// match `10`". The prefix length determines specificity: a `/24` is more specific
    /// than a `/8` and wins in a lookup when both match.
    ///
    /// If an entry already exists for this prefix, its value is replaced.
    ///
    /// Any host bits set in the prefix address are ignored — `10.99.0.0/8` and
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
    /// table.insert("10.20.0.0/16".parse().unwrap(), "specific");
    ///
    /// // Inserting the same prefix again replaces the old value.
    /// table.insert("10.0.0.0/8".parse().unwrap(), "updated");
    /// assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"updated"));
    /// ```
    pub fn insert(&mut self, prefix: IpPrefix<A>, value: V) {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32; // Prefix length
        let mut node = &mut self.root;

        for depth in 0..len {
            //  1. Extract the current bit — this is the left/right decision at this depth
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;

            // 2. Create the child if it doesn't exist
            node = node.children[bit]
                .get_or_insert_with(|| Box::new(TrieNode::new()))
                .as_mut();
        }

        node.value = Some(value);
    }

    /// Returns a reference to the value for the most specific prefix that contains
    /// `addr`, or `None` if no prefix in the table matches.
    ///
    /// "Most specific" means the matching prefix with the longest prefix length.
    /// If the table contains `10.0.0.0/8` and `10.20.0.0/16`, a lookup for
    /// `10.20.5.1` returns the value for `10.20.0.0/16` — it covers a smaller
    /// portion of address space, making it more precise.
    ///
    /// A default route (`0.0.0.0/0` for IPv4, `::/0` for IPv6) matches every
    /// address and acts as a catch-all fallback when no more specific prefix matches.
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
    /// // Most specific match wins.
    /// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"third-floor"));
    ///
    /// // Falls back to the next most specific match.
    /// assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"datacenter"));
    ///
    /// // Falls back to the default route when nothing more specific matches.
    /// assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"default"));
    /// ```
    pub fn longest_match(&self, addr: A) -> Option<&V> {
        let addr = addr.to_u128(); // The bits to walk
        let mut node = &self.root;
        let mut best = node.value.as_ref(); // handles a /0 prefix at root

        for depth in 0..A::BITS as u32 {
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
            if let Some(child) = node.children[bit].as_deref() {
                // Advance the node
                node = child;
                // Update 'best' if this node has a value
                if let Some(value) = child.value.as_ref() {
                    best = Some(value);
                }
            } else {
                break;
            }
        }
        best
    }

    /// Removes the entry for `prefix` from the table and returns its value, or
    /// `None` if no such entry exists.
    ///
    /// The table is left unchanged if the prefix is not found.
    ///
    /// Any host bits set in the prefix address are ignored — `remove("10.99.0.0/8")`
    /// will find and remove the entry stored under `10.0.0.0/8`.
    ///
    /// Removing a broad prefix does not affect more specific prefixes nested beneath
    /// it. Removing a specific prefix restores visibility to any broader prefix that
    /// previously covered the same addresses.
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
    /// // Removing the specific prefix returns its value and falls back to the broader one.
    /// assert_eq!(table.remove("10.20.0.0/16".parse().unwrap()), Some("specific"));
    /// assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), Some(&"broad"));
    ///
    /// // Removing a prefix that was never inserted returns None.
    /// assert_eq!(table.remove("192.168.0.0/16".parse().unwrap()), None);
    /// ```
    pub fn remove(&mut self, prefix: IpPrefix<A>) -> Option<V> {
        let prefix = prefix.masked();
        let (value, _) = Self::remove_recursive(
            &mut self.root,
            prefix.ip().to_u128(),
            0,
            prefix.mask() as u32,
        );

        value
    }

    /// Returns `true` if the table contains an entry for exactly this prefix.
    ///
    /// This checks for an exact prefix match — not whether an address falls within
    /// any stored prefix. For example, if the table contains `10.0.0.0/8`, then
    /// `contains("10.0.0.0/8")` returns `true`, but `contains("10.20.0.0/16")`
    /// returns `false` even though `10.20.x.x` addresses would match via
    /// [`longest_match`](IpTable::longest_match).
    ///
    /// Any host bits set in the prefix address are ignored — `contains("10.99.0.0/8")`
    /// and `contains("10.0.0.0/8")` are equivalent.
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
    /// // Exact prefix match.
    /// assert!(table.contains("10.0.0.0/8".parse().unwrap()));
    ///
    /// // A more specific prefix that was never inserted returns false,
    /// // even though 10.20.x.x addresses match via longest_match.
    /// assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    /// ```
    pub fn contains(&self, prefix: IpPrefix<A>) -> bool {
        let mut node = &self.root;
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32; // Prefix length

        for depth in 0..len {
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
            if let Some(child) = node.children[bit].as_deref() {
                node = child
            } else {
                return false;
            }
        }

        node.value.is_some()
    }

    // Recursion depth is bounded by A::BITS — 32 for IPv4, 128 for IPv6.
    // This is a hard constant regardless of table size, so stack overflow is
    // not a concern even for very large tables.
    fn remove_recursive(
        node: &mut TrieNode<V>,
        addr: u128,
        depth: u32,
        target_depth: u32,
    ) -> (Option<V>, bool) {
        if depth == target_depth {
            let value = node.value.take();
            let empty = node.children[0].is_none() && node.children[1].is_none();
            return (value, empty);
        }

        let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;

        if let Some(child) = node.children[bit].as_deref_mut() {
            let (value, prune_child) = Self::remove_recursive(child, addr, depth + 1, target_depth);
            if prune_child {
                node.children[bit] = None;
            }

            let this_empty =
                node.value.is_none() && node.children[0].is_none() && node.children[1].is_none();

            (value, this_empty)
        } else {
            (None, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ── Empty table ──────────────────────────────────────────────────────────
    // A freshly created table should never return a match.

    #[test]
    fn empty_table_returns_none() {
        let table: IpTable<Ipv4Addr, &str> = IpTable::new();
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    // ── Default route /0 ─────────────────────────────────────────────────────
    // A /0 prefix covers the entire address space. It lives at the root node,
    // exercising the `best = node.value.as_ref()` initialisation before the loop.

    #[test]
    fn default_route_matches_any_address() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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

    // ── Single prefix ────────────────────────────────────────────────────────
    // Basic hit and miss, plus boundary addresses.

    #[test]
    fn single_prefix_hit() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn single_prefix_miss() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn network_address_itself_matches() {
        // The network address (host bits all zero) must match its own prefix.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.0".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn address_just_outside_prefix_misses() {
        // 11.0.0.0 shares no bits with 10.0.0.0/8 after the first divergence.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.0".parse().unwrap()), None);
    }

    #[test]
    fn last_address_in_prefix_matches() {
        // The broadcast address (host bits all one) must also match.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/24".parse().unwrap(), "subnet");
        assert_eq!(
            table.longest_match("10.0.0.255".parse().unwrap()),
            Some(&"subnet")
        );
    }

    // ── Most specific wins ───────────────────────────────────────────────────
    // The core LPM guarantee: the deepest matching prefix is returned.

    #[test]
    fn most_specific_prefix_wins() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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
        // Verifies the best-match register updates correctly at each depth.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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

    // ── /32 exact host match ─────────────────────────────────────────────────
    // A /32 is the most specific IPv4 prefix — a single host address.

    #[test]
    fn slash32_matches_only_that_host() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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

    // ── Overwrite ────────────────────────────────────────────────────────────
    // Inserting the same prefix twice replaces the stored value.

    #[test]
    fn inserting_same_prefix_twice_overwrites_value() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"second")
        );
    }

    // ── Non-overlapping prefixes ─────────────────────────────────────────────
    // Two disjoint prefixes must not bleed into each other.

    #[test]
    fn non_overlapping_prefixes_do_not_cross_match() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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

    // ── IPv6 ─────────────────────────────────────────────────────────────────
    // The same logic must hold for 128-bit addresses.

    #[test]
    fn ipv6_basic_match() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
        table.insert("2001:db8::/32".parse().unwrap(), "docs");
        assert_eq!(
            table.longest_match("2001:db8::1".parse().unwrap()),
            Some(&"docs")
        );
        assert_eq!(table.longest_match("2001:db9::1".parse().unwrap()), None);
    }

    #[test]
    fn ipv6_most_specific_wins() {
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
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
        let mut table: IpTable<Ipv6Addr, &str> = IpTable::new();
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

    // ── remove ───────────────────────────────────────────────────────────────

    #[test]
    fn remove_returns_the_value() {
        // remove() should return the stored value, not just delete it silently.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.0.0.0/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_makes_prefix_unmatchable() {
        // After removal, longest_match should return None for addresses that
        // previously matched the removed prefix.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_nonexistent_prefix_returns_none() {
        // Removing a prefix that was never inserted should return None
        // and leave the table intact.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("192.168.0.0/16".parse().unwrap()), None);
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn remove_specific_falls_back_to_general() {
        // Removing the more specific prefix should expose the broader one again.
        // This verifies pruning does not damage the parent node's value.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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
        // Removing the broader prefix should not affect the more specific one.
        // Addresses inside the specific prefix still match; others return None.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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
        // remove() calls .masked() internally, so host bits in the prefix
        // address should not prevent finding the stored entry.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.99.99.99/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_default_route() {
        // The /0 entry lives at the root node — verify it can be removed
        // without breaking lookups for other prefixes.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
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
        // Removing every prefix should leave the table in the same state
        // as a freshly created one — all lookups return None.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.insert("10.20.0.0/16".parse().unwrap(), "b");

        table.remove("10.0.0.0/8".parse().unwrap());
        table.remove("10.20.0.0/16".parse().unwrap());

        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), None);
    }

    // ── contains ─────────────────────────────────────────────────────────────

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
        // This is the key distinction between contains and longest_match.
        // longest_match("10.20.5.1") would return "ten" via the /8 entry,
        // but contains("10.20.0.0/16") is false — that prefix was never inserted.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_broader_prefix_that_was_not_inserted() {
        // A more specific prefix being present does not imply the broader one is.
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
        // Host bits in the prefix address are ignored.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.99.99.99/8".parse().unwrap()));
    }

    #[test]
    fn empty_table_contains_nothing() {
        let table: IpTable<Ipv4Addr, &str> = IpTable::new();
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }
}
