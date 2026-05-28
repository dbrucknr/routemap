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
}
