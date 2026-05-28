use crate::node::TrieNode;
use ipnetx::{interfaces::IpAddress, prefix::IpPrefix};
use std::marker::PhantomData;

// Should we consider a static dispatch for the generic V value?

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
        let p = prefix.masked();
        let ip = p.ip().to_u128();
        let mask = p.mask() as u32;

        let mut node = &mut self.root;

        for depth in 0..mask {
            //  1. Extract the current bit — this is the left/right decision at this depth
            let bit = ((ip >> (A::BITS as u32 - 1 - depth)) & 1) as usize;

            // 2. Create the child if it doesn't exist
            node = node.children[bit]
                .get_or_insert_with(|| Box::new(TrieNode::new()))
                .as_mut();
        }

        node.value = Some(value);
    }
}
