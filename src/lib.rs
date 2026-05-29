#![doc = include_str!("../README.md")]

// Archived arena-based binary trie kept for benchmark comparison.
// Not part of the public API — RouteMap (treebitmap) is the only exported type.
#[allow(dead_code)]
mod arena;
mod treebitmap;
pub use treebitmap::RouteMap;
