use crate::node::TrieNode;
use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::marker::PhantomData;

pub struct IpTable<A: IpAddress, V> {
    root: TrieNode<V>,
    _marker: PhantomData<A>,
}
impl<A: IpAddress, V> IpTable<A, V> {
    pub fn new() -> Self {
        Self {
            root: TrieNode::new(),
            _marker: PhantomData,
        }
    }

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

    // ── Single prefix ────────────────────────────────────────────────────────
    // Basic hit and miss, plus boundary addresses.

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
        // The network address (host bits all zero) must match its own prefix.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("10.0.0.0".parse().unwrap()), Some(&"ten"));
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
        assert_eq!(table.longest_match("10.0.0.255".parse().unwrap()), Some(&"subnet"));
    }

    // ── Most specific wins ───────────────────────────────────────────────────
    // The core LPM guarantee: the deepest matching prefix is returned.

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
        // Verifies the best-match register updates correctly at each depth.
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "level-1");
        table.insert("10.20.0.0/16".parse().unwrap(), "level-2");
        table.insert("10.20.30.0/24".parse().unwrap(), "level-3");
        assert_eq!(table.longest_match("10.20.30.1".parse().unwrap()), Some(&"level-3"));
        assert_eq!(table.longest_match("10.20.99.1".parse().unwrap()), Some(&"level-2"));
        assert_eq!(table.longest_match("10.99.0.1".parse().unwrap()), Some(&"level-1"));
        assert_eq!(table.longest_match("9.0.0.1".parse().unwrap()), None);
    }

    // ── /32 exact host match ─────────────────────────────────────────────────
    // A /32 is the most specific IPv4 prefix — a single host address.

    #[test]
    fn slash32_matches_only_that_host() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "broad");
        table.insert("10.0.0.1/32".parse().unwrap(), "host");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"host"));
        assert_eq!(table.longest_match("10.0.0.2".parse().unwrap()), Some(&"broad"));
    }

    // ── Overwrite ────────────────────────────────────────────────────────────
    // Inserting the same prefix twice replaces the stored value.

    #[test]
    fn inserting_same_prefix_twice_overwrites_value() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), Some(&"second"));
    }

    // ── Non-overlapping prefixes ─────────────────────────────────────────────
    // Two disjoint prefixes must not bleed into each other.

    #[test]
    fn non_overlapping_prefixes_do_not_cross_match() {
        let mut table: IpTable<Ipv4Addr, &str> = IpTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.insert("192.168.0.0/16".parse().unwrap(), "office");
        assert_eq!(table.longest_match("10.1.2.3".parse().unwrap()), Some(&"ten"));
        assert_eq!(table.longest_match("192.168.1.1".parse().unwrap()), Some(&"office"));
        assert_eq!(table.longest_match("172.16.0.1".parse().unwrap()), None);
    }

    // ── IPv6 ─────────────────────────────────────────────────────────────────
    // The same logic must hold for 128-bit addresses.

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
}
