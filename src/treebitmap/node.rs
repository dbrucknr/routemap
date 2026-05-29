pub(crate) struct TbNode<V> {
    /// Bits 1–15: which internal prefix positions are occupied (stride-4 binary heap).
    pub(crate) internal: u32,
    /// Bits 0–15: which child nodes exist (one bit per possible nibble value).
    pub(crate) external: u32,
    /// Compact value store — length equals internal.count_ones().
    pub(crate) values: Vec<V>,
    /// Compact child store — length equals external.count_ones().
    pub(crate) children: Vec<TbNode<V>>,
}

impl<V> TbNode<V> {
    pub(crate) fn new() -> Self {
        Self {
            internal: 0,
            external: 0,
            values: Vec::new(),
            children: Vec::new(),
        }
    }
}
