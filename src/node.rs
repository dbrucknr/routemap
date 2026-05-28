pub(crate) struct TrieNode<V> {
    pub(crate) children: [Option<Box<TrieNode<V>>>; 2],
    pub(crate) value: Option<V>,
}

impl<V> TrieNode<V> {
    pub(crate) fn new() -> Self {
        Self {
            children: [None, None],
            value: None,
        }
    }
}
