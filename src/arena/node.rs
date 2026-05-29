pub(crate) const NULL: u32 = u32::MAX;

pub(crate) struct ArenaNode<V> {
    pub(crate) children: [u32; 2],
    pub(crate) value: Option<V>,
}

impl<V> ArenaNode<V> {
    pub(crate) fn new() -> Self {
        Self {
            children: [NULL, NULL],
            value: None,
        }
    }
}
