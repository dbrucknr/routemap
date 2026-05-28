Claude provided documentation about how we use ipnetx to solve the LPM problem:

1.1
What ipnetx gives us

There are two types we'll use from ipnetx. Let's look at each.

IpAddress — the sealed trait

This is implemented by Ipv4Addr and Ipv6Addr only (the sealed trait pattern means nobody outside the crate can implement it). The three methods that
matter to us:

const BITS: u8;              // 32 for IPv4, 128 for IPv6
fn to_u128(self) -> u128;    // the whole address as a number
fn from_u128(bits: u128) -> Self;  // build an address from a number

BITS tells us how deep the trie goes. to_u128() is the bridge between an IP address and a sequence of bit decisions.

One subtlety worth noting: IPv4 addresses are stored in the low 32 bits of the u128. 10.0.0.0 → 0x0000_0000_0000_0000_0000_0000_0A00_0000. IPv6 uses
all 128 bits. The formula we'll derive handles both automatically via A::BITS.

IpPrefix<A> — the key type

Two fields, two methods we care about:

fn ip(&self) -> A     // the address portion
fn mask(&self) -> u8  // the prefix length: 0..=A::BITS
fn masked(&self) -> Self  // returns the prefix with host bits zeroed

The masked() method is important and we'll come back to it.

---
Deriving the bit extraction formula

The trie makes a left/right decision at each level based on one bit of the address. Level 0 uses bit 0 (the most significant bit), level 1 uses bit 1,
  and so on.

For IPv4, 10.0.0.0 in binary is:
00001010  00000000  00000000  00000000
^bit 0                             bit 31^

To extract bit N (counting from 0 at the most significant bit), we need to:
1. Shift the number right so that bit N lands in the ones position
2. Mask off everything except that last bit

The shift amount is (A::BITS - 1 - N). For IPv4 at bit 0: shift right by 31. At bit 31: shift right by 0.

let bit = (addr.to_u128() >> (A::BITS as u32 - 1 - n)) & 1;

Let's verify with a real example. 10.0.0.0 as u32 = 0x0A000000 = 167772160.

- Bit 0 (MSB): shift right by 31 → 167772160 >> 31 = 0. Correct — the MSB of 10 (00001010) is 0.
- Bit 4: shift right by 27 → 167772160 >> 27 = 1. Correct — that's the 1 in 0000**1**010.
- Bit 7: shift right by 24 → result & 1 = 0. Correct — 0000101**0**.

The formula works for IPv6 identically because to_u128() uses all 128 bits and A::BITS is 128.

---
Why masked() matters for insert

IpPrefix::new() deliberately preserves host bits. Someone can construct 10.0.0.1/8 — the address has host bits set, but the prefix length says only 8
bits matter. If we insert that into the trie naively, we'd walk 8 bits of 00001010 correctly, but when we later reconstruct the prefix for iter() we'd
  rebuild 10.0.0.1/8 instead of 10.0.0.0/8.

We fix this by calling .masked() on every prefix before inserting it. That zeros the host bits:

10.0.0.1/8  →  .masked()  →  10.0.0.0/8

From that point on, the stored prefix is canonical and reconstruction is unambiguous.

---
What the trie will actually use

Summarising the four things our implementation will call into ipnetx for:

┌───────────────────────┬──────────────────────────────────────────────────────────────────┐
│         Call          │                         Where we use it                          │
├───────────────────────┼──────────────────────────────────────────────────────────────────┤
│ prefix.ip().to_u128() │ Extract individual bits during insert and lookup                 │
├───────────────────────┼──────────────────────────────────────────────────────────────────┤
│ prefix.mask()         │ Know when to stop during insert (depth = prefix length)          │
├───────────────────────┼──────────────────────────────────────────────────────────────────┤
│ A::BITS               │ Know the full trie depth for lookup; bit extraction shift amount │
├───────────────────────┼──────────────────────────────────────────────────────────────────┤
│ prefix.masked()       │ Canonicalise on insert to ensure consistent reconstruction       │
└───────────────────────┴──────────────────────────────────────────────────────────────────┘

That's the complete interface. The trie itself is pure Rust — ipnetx just hands us integers and constants.

--
For 2.1 we're creating src/node.rs — the foundational building block of the trie. Before writing, let me explain the three design decisions so the
code reads as obvious rather than arbitrary.

Why [Option<Box<TrieNode<V>>>; 2] instead of left and right fields?
Because the bit extraction formula gives us 0 or 1 directly. children[bit] where bit is 0 or 1 lets us index without a branch — cleaner insert and
lookup code.

Why Box?
Rust requires that every type has a known size at compile time. TrieNode<V> can't contain TrieNode<V> directly — that would be infinite size.
Box<TrieNode<V>> is just a pointer, which has a fixed size. This is the standard Rust pattern for any recursive data structure.

Why Option<V> and not just V?
Not every node marks a prefix endpoint. Intermediate nodes exist only to connect two prefixes that share a common bit prefix — they have no value of
their own. None means "structural node only"; Some(v) means "a prefix ends here."

---
3.1 — Two new concepts before writing IpTable

Why two type parameters: IpTable<A, V>

V is the value type — the thing you're storing (route entry, geo record, etc.). A is the address family — Ipv4Addr or Ipv6Addr. We put A on the struct
 so the type system enforces that a table is dedicated to one address family. You can't accidentally insert an IPv6 prefix into an IPv4 table — the
compiler rejects it.

PhantomData<A> — a new pattern

A appears in method signatures (insert takes an IpPrefix<A>) but isn't stored in any field. Rust requires that every declared type parameter actually
appear in a field, otherwise it can't reason about ownership and lifetimes. PhantomData<A> is the solution — it's a zero-sized type that vanishes
completely at runtime but satisfies the compiler. The _ prefix on _marker signals to Rust not to warn about an "unused" field.

use std::marker::PhantomData;

pub struct IpTable<A: IpAddress, V> {
    root: TrieNode<V>,
    _marker: PhantomData<A>,
}

The insert algorithm

Here's the logic in plain English before you write it:

1. Call .masked() on the prefix — canonicalise it (zeroes host bits)
2. Pull out the address bits with .ip().to_u128()
3. Pull out the depth limit with .mask() as u32
4. Start at self.root with a mutable reference called node
5. Loop depth from 0 to len (exclusive):
  - Extract the current bit: ((addr >> (A::BITS as u32 - 1 - depth)) & 1) as usize — gives you 0 or 1
  - If node.children[bit] is None, create a new node there
  - Step down: reassign node to node.children[bit].as_mut().unwrap()
6. After the loop, set node.value = Some(value)

The loop runs exactly prefix_len times — for a /8 that's 8 iterations, placing you at depth 8. The value is stored there. The remaining bits of the
address are irrelevant for this prefix.

  ---
  4.1 — Why remove needs recursion
  
  With insert and longest_match, walking down the tree in a loop was natural — you always move forward. remove has an extra requirement: pruning.

  After you take a value out, the node it lived in might now be dead weight — no value, no children. You should delete it. But its parent might now also
   be dead weight. And its grandparent too. You can only know whether to delete a node after you've processed everything below it.

  That's a post-order problem — act on the way back up. Recursion expresses this naturally: each call returns a signal to its caller saying "the child
  you sent me into is now empty — you can delete it."

  The shape:

  remove_recursive(node, addr, depth, target_depth):
      if depth == target_depth:
          take the value out of node
          return (taken_value, node_is_now_empty)

      bit = extract bit at this depth
      if child[bit] exists:
          (value, should_prune) = recurse into child
          if should_prune → delete child[bit]
          return (value, this_node_is_now_empty)
      else:
          prefix wasn't in the table → return (None, false)

  One new method to know: Option::take() — removes the value from an Option in place and returns it, leaving None behind:
  let v = node.value.take(); // node.value is now None, v is the old Option<V>
  
  "Node is now empty" means: node.value.is_none() && node.children[0].is_none() && node.children[1].is_none()

  The public remove method calls the recursive helper and ignores the prune signal for the root — the root node is never deleted, it always stays.
