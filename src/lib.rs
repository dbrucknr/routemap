#![doc = include_str!("../README.md")]

// Archived arena-based binary trie kept for benchmark comparison.
// Not part of the public API — IpTable (treebitmap) is the only exported type.
mod arena;
mod treebitmap;
pub use treebitmap::IpTable;
