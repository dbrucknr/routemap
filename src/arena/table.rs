use crate::arena::node::{ArenaNode, NULL};
use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::marker::PhantomData;

pub struct ArenaTable<A: IpAddress, V> {
    nodes: Vec<ArenaNode<V>>,
    _marker: PhantomData<A>,
}

impl<A: IpAddress, V> ArenaTable<A, V> {
    pub fn new() -> Self {
        Self {
            nodes: vec![ArenaNode::new()],
            _marker: PhantomData,
        }
    }

    pub fn insert(&mut self, prefix: IpPrefix<A>, value: V) {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        let mut cur = 0usize;

        for depth in 0..len {
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
            let child_idx = self.nodes[cur].children[bit];
            let next = if child_idx == NULL {
                let new_idx = self.nodes.len() as u32;
                self.nodes.push(ArenaNode::new());
                self.nodes[cur].children[bit] = new_idx;
                new_idx as usize
            } else {
                child_idx as usize
            };
            cur = next;
        }

        self.nodes[cur].value = Some(value);
    }

    pub fn longest_match(&self, addr: A) -> Option<&V> {
        let addr = addr.to_u128();
        let mut cur = 0usize;
        let mut best = self.nodes[0].value.as_ref();

        for depth in 0..A::BITS as u32 {
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
            let child_idx = self.nodes[cur].children[bit];
            if child_idx == NULL {
                break;
            }
            cur = child_idx as usize;
            if let Some(v) = self.nodes[cur].value.as_ref() {
                best = Some(v);
            }
        }

        best
    }

    pub fn remove(&mut self, prefix: IpPrefix<A>) -> Option<V> {
        let prefix = prefix.masked();
        let (value, _) = Self::remove_recursive(
            &mut self.nodes,
            0,
            prefix.ip().to_u128(),
            0,
            prefix.mask() as u32,
        );
        value
    }

    pub fn contains(&self, prefix: IpPrefix<A>) -> bool {
        let prefix = prefix.masked();
        let addr = prefix.ip().to_u128();
        let len = prefix.mask() as u32;
        let mut cur = 0usize;

        for depth in 0..len {
            let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
            let child_idx = self.nodes[cur].children[bit];
            if child_idx == NULL {
                return false;
            }
            cur = child_idx as usize;
        }

        self.nodes[cur].value.is_some()
    }

    // Recursion depth is bounded by A::BITS — 32 for IPv4, 128 for IPv6.
    fn remove_recursive(
        nodes: &mut Vec<ArenaNode<V>>,
        cur: u32,
        addr: u128,
        depth: u32,
        target_depth: u32,
    ) -> (Option<V>, bool) {
        if depth == target_depth {
            let value = nodes[cur as usize].value.take();
            let empty =
                nodes[cur as usize].children[0] == NULL && nodes[cur as usize].children[1] == NULL;
            return (value, empty);
        }

        let bit = ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize;
        let child_idx = nodes[cur as usize].children[bit];

        if child_idx == NULL {
            return (None, false);
        }

        let (value, prune_child) =
            Self::remove_recursive(nodes, child_idx, addr, depth + 1, target_depth);

        if prune_child {
            nodes[cur as usize].children[bit] = NULL;
        }

        let this_empty = nodes[cur as usize].value.is_none()
            && nodes[cur as usize].children[0] == NULL
            && nodes[cur as usize].children[1] == NULL;

        (value, this_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn empty_table_returns_none() {
        let table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn default_route_matches_any_address() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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

    #[test]
    fn single_prefix_hit() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn single_prefix_miss() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn network_address_itself_matches() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(
            table.longest_match("10.0.0.0".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn address_just_outside_prefix_misses() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.longest_match("11.0.0.0".parse().unwrap()), None);
    }

    #[test]
    fn last_address_in_prefix_matches() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/24".parse().unwrap(), "subnet");
        assert_eq!(
            table.longest_match("10.0.0.255".parse().unwrap()),
            Some(&"subnet")
        );
    }

    #[test]
    fn most_specific_prefix_wins() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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

    #[test]
    fn slash32_matches_only_that_host() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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

    #[test]
    fn inserting_same_prefix_twice_overwrites_value() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "first");
        table.insert("10.0.0.0/8".parse().unwrap(), "second");
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"second")
        );
    }

    #[test]
    fn non_overlapping_prefixes_do_not_cross_match() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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

    #[test]
    fn ipv6_basic_match() {
        let mut table: ArenaTable<Ipv6Addr, &str> = ArenaTable::new();
        table.insert("2001:db8::/32".parse().unwrap(), "docs");
        assert_eq!(
            table.longest_match("2001:db8::1".parse().unwrap()),
            Some(&"docs")
        );
        assert_eq!(table.longest_match("2001:db9::1".parse().unwrap()), None);
    }

    #[test]
    fn ipv6_most_specific_wins() {
        let mut table: ArenaTable<Ipv6Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv6Addr, &str> = ArenaTable::new();
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

    #[test]
    fn remove_returns_the_value() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.0.0.0/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_makes_prefix_unmatchable() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn remove_nonexistent_prefix_returns_none() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("192.168.0.0/16".parse().unwrap()), None);
        assert_eq!(
            table.longest_match("10.0.0.1".parse().unwrap()),
            Some(&"ten")
        );
    }

    #[test]
    fn remove_specific_falls_back_to_general() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert_eq!(table.remove("10.99.99.99/8".parse().unwrap()), Some("ten"));
    }

    #[test]
    fn remove_default_route() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
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
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "a");
        table.insert("10.20.0.0/16".parse().unwrap(), "b");
        table.remove("10.0.0.0/8".parse().unwrap());
        table.remove("10.20.0.0/16".parse().unwrap());
        assert_eq!(table.longest_match("10.0.0.1".parse().unwrap()), None);
        assert_eq!(table.longest_match("10.20.5.1".parse().unwrap()), None);
    }

    #[test]
    fn contains_inserted_prefix() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_uninserted_prefix() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("192.168.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_more_specific_prefix_that_was_not_inserted() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(!table.contains("10.20.0.0/16".parse().unwrap()));
    }

    #[test]
    fn does_not_contain_broader_prefix_that_was_not_inserted() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.20.0.0/16".parse().unwrap(), "specific");
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_false_after_remove() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        table.remove("10.0.0.0/8".parse().unwrap());
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }

    #[test]
    fn contains_with_unmasked_prefix() {
        let mut table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        table.insert("10.0.0.0/8".parse().unwrap(), "ten");
        assert!(table.contains("10.99.99.99/8".parse().unwrap()));
    }

    #[test]
    fn empty_table_contains_nothing() {
        let table: ArenaTable<Ipv4Addr, &str> = ArenaTable::new();
        assert!(!table.contains("10.0.0.0/8".parse().unwrap()));
    }
}
