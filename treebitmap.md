# Treebitmap Algorithm Reference

Source: Eatherton, Varghese, Bhatt — "Tree Bitmap: Hardware/Software IP Lookups
with Incremental Updates" (2004). Reference implementation: hroi/treebitmap.

---

## Why the binary trie is slow

Every hop in a binary trie follows one pointer to a separately-allocated node.
A lookup on a `/24` prefix makes 24 pointer dereferences into 24 different memory
locations. At 100k prefixes, those nodes are spread across the heap — nearly every
dereference is a cache miss.

The treebitmap attacks this directly: **process multiple bits per node** (called a
*stride*), so fewer nodes means fewer cache misses. With stride 4, a `/24` takes
6 hops instead of 24.

---

## The stride insight

With stride 4, each node owns a 4-bit window of the address. There are two kinds
of things a node tracks:

- **Internal prefixes** — prefixes that *end within* the node's 4-bit window
  (relative lengths 0, 1, 2, or 3 bits)
- **External children** — pointers to the next node, one per possible 4-bit value
  (up to 16)

A naïve implementation would pre-allocate 15 value slots and 16 child slots per
node, almost all empty. Treebitmap instead stores only the *occupied* entries
compactly, and uses bitmaps + popcount to find them.

---

## Node structure

```rust
struct TbNode<V> {
    internal: u32,      // 15-bit bitmap — which internal prefix positions are occupied
    external: u32,      // 16-bit bitmap — which child pointers exist
    values:   Vec<V>,   // compact — only occupied entries, no gaps
    children: Vec<TbNode<V>>, // compact — only occupied entries, no gaps
}
```

---

## The internal bitmap

For stride 4, the 15 possible internal prefix positions are arranged as a complete
binary tree (standard binary heap indexing, 1-based):

```
                       [1]                ← length 0 relative (1 prefix)
                /             \
            [2]                 [3]       ← length 1 (0_ and 1_)
           /   \               /   \
         [4]   [5]           [6]   [7]   ← length 2 (00_, 01_, 10_, 11_)
        / \   / \           / \   / \
      [8] [9][10][11]   [12][13][14][15] ← length 3 (000_, 001_, ..., 111_)
```

A set bit at position `p` means a prefix is stored at that position. Given a 4-bit
nibble `n = b₃b₂b₁b₀`, the positions to check during a lookup are:

| Relative length | Bitmap position | Bits matched |
|---|---|---|
| 0 | `1` | everything in this stride |
| 1 | `2 + b₃` | top 1 bit of nibble |
| 2 | `4 + (n >> 2)` | top 2 bits of nibble |
| 3 | `8 + (n >> 1)` | top 3 bits of nibble |

The general formula for (relative_length `l`, relative_value `v`):

```
bitmap_position = (1 << l) + v
```

---

## The external bitmap

16 bits, one per possible 4-bit value (0..15). Bit `n` set means a child node
exists for nibble `n`. That child is the root of the sub-trie covering all
addresses whose next 4 bits are `n`.

---

## The popcount-rank trick

Neither `values` nor `children` has empty slots — they only hold occupied entries.
To find which index a given bitmap position maps to, count how many set bits appear
*before* it:

```rust
let index = (bitmap & ((1 << position) - 1)).count_ones() as usize;
```

Example — `internal = 0b000000001001010` (bits set at positions 1, 3, 6):

```
Lookup position 6:
  mask  = (1 << 6) - 1 = 0b000000000111111
  masked = internal & mask = 0b000000000001010   (bits 1 and 3)
  popcount = 2
  → value is at values[2]
```

`count_ones()` compiles to a single `POPCNT` instruction on x86 and ARM.

---

## Concrete walkthrough

IPv4, stride 4. Three prefixes inserted:

- `0.0.0.0/0`    → `"default"`
- `10.0.0.0/8`   → `"datacenter"` (binary: `0000·1010·…`)
- `10.20.0.0/16` → `"third-floor"` (binary: `0000·1010·0001·0100·…`)

### Node layout after insert

**Node 0** (root, covers bits 0–3):
- `0.0.0.0/0`: length 0 prefix → internal bit 1 set
- `10.0.0.0/8` and `10.20.0.0/16`: first nibble is `0000` → external bit 0 set

```
internal = 0b000000000000010  (bit 1)   values   = ["default"]
external = 0b0000000000000001 (bit 0)   children = [Node 1]
```

**Node 1** (covers bits 4–7, reached via nibble `0000`):
- Both remaining prefixes: second nibble is `1010` = 10 → external bit 10 set

```
internal = 0                            values   = []
external = 0b0000010000000000 (bit 10)  children = [Node 2]
```

**Node 2** (covers bits 8–11, reached via nibble `1010`):
- `10.0.0.0/8`: length 8 = 2×4, ends at relative length 0 → internal bit 1
- `10.20.0.0/16`: third nibble is `0001` = 1 → external bit 1 set

```
internal = 0b000000000000010  (bit 1)   values   = ["datacenter"]
external = 0b0000000000000010 (bit 1)   children = [Node 3]
```

**Node 3** (covers bits 12–15, reached via nibble `0001`):
- `10.20.0.0/16`: length 16 = 4×4, ends at relative length 0 → internal bit 1

```
internal = 0b000000000000010  (bit 1)   values   = ["third-floor"]
external = 0                            children = []
```

### Lookup: `10.20.5.1` (nibbles: `0000·1010·0001·0100·…`)

1. **Node 0**, nibble `0000`:
   - Check internal positions 1, 2, 4, 8 → bit 1 set → best = `"default"`
   - Check external bit 0 → set → go to Node 1
2. **Node 1**, nibble `1010`:
   - Check internal positions 1, 3, 6, 13 → none set
   - Check external bit 10 → set → go to Node 2
3. **Node 2**, nibble `0001`:
   - Check internal positions 1, 2, 4, 8 → bit 1 set → best = `"datacenter"`
   - Check external bit 1 → set → go to Node 3
4. **Node 3**, nibble `0100`:
   - Check internal positions 1, 2, 5, 10 → bit 1 set → best = `"third-floor"`
   - Check external bit 4 → not set → stop

Result: `"third-floor"`. 4 node hops instead of 16.

---

## Insert algorithm

For a prefix of length `L` with address bits `addr`:

1. **Navigate** to the correct node: take `L / stride` full stride hops, creating
   child nodes as needed (via the external bitmap + popcount to find/insert into
   the children Vec)
2. **Compute relative position** within the destination node:
   - `rel_len = L % stride` (0..stride)
   - `rel_bits = (addr >> (total_bits - depth*stride - rel_len)) & ((1 << rel_len) - 1)`
   - `bitmap_pos = (1 << rel_len) + rel_bits`
3. **Insert the value** at the rank-computed index in `values`:
   - `idx = (internal & ((1 << bitmap_pos) - 1)).count_ones()`
   - `values.insert(idx, value)`
4. **Set the bitmap bit**: `internal |= 1 << bitmap_pos`

For navigation (step 1): at each stride hop, extract the current nibble, check the
external bitmap to find/create the child, then advance. If creating a child: insert
a new node into `children` at the rank position and set the external bit.

**Key cost**: `values.insert(idx, value)` is O(n) for the values in that node. In
practice nodes hold very few values (real BGP tables average < 1 internal prefix
per node), so this is fast.

---

## Remove algorithm

1. Navigate to the node (same as insert), collecting the path of (node, nibble)
   pairs
2. Compute `bitmap_pos` and `idx` as in insert
3. `values.remove(idx)` and clear the bitmap bit: `internal &= !(1 << bitmap_pos)`
4. If the node is now empty (internal == 0 && external == 0), signal the parent to
   prune it: `children.remove(child_idx)` and clear the external bit

---

## What to implement first

The popcount-rank function is the primitive everything else is built on:

```rust
fn rank(bitmap: u32, position: u32) -> usize {
    (bitmap & ((1 << position) - 1)).count_ones() as usize
}
```

Get this tested in isolation before touching `IpTable`. Any bug here silently
corrupts every insert and lookup.

---

## Benchmark results

Recorded 2026-05-29 on Apple M2 Max, Rust 1.94.0. Full results in BENCHMARKS.md.

### Lookup throughput at 100k prefixes

| | IPv4 thrpt | IPv6 thrpt |
|---|---|---|
| Binary trie (baseline) | 18.6 M/s | 13.4 M/s |
| Treebitmap (stride-4)  | 25.1 M/s | 47.4 M/s |
| Speedup                | **1.35×** | **3.54×** |

IPv6 benefits far more: the binary trie makes up to 128 hops for a /128;
treebitmap caps at 32. At 100k prefixes (mostly L3 cache misses), cutting hops
by 4× produces a 3.5× wall-clock speedup. IPv4 gains are more modest (max hops
32 → 8) at 1.35×.

Insert is 2× slower at small sizes due to `Vec::insert()` cost for keeping the
compact arrays sorted, but converges to ≈ parity at 100k prefixes.
