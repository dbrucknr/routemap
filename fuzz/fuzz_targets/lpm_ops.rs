#![no_main]

use arbitrary::Arbitrary;
use ipnetx::prefix::IpPrefix;
use libfuzzer_sys::fuzz_target;
use routemap::RouteMap;
use std::net::Ipv4Addr;

/// A single operation to apply to the table.
///
/// `arbitrary` drives the fuzzer: it decodes raw fuzzer bytes into a Vec<Op>
/// using the derived Arbitrary impl, so every reachable combination of
/// operations and address/length values gets exercised automatically.
#[derive(Arbitrary, Debug)]
enum Op {
    Insert { addr: u32, len: u8, val: u32 },
    Remove { addr: u32, len: u8 },
    LongestMatch { addr: u32 },
    LongestMatchEntry { addr: u32 },
    Get { addr: u32, len: u8 },
    Contains { addr: u32, len: u8 },
    Clear,
    Len,
    IsEmpty,
}

fuzz_target!(|ops: Vec<Op>| {
    let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();

    for op in ops {
        match op {
            Op::Insert { addr, len, val } => {
                // Clamp len to 0..=32; IpPrefix::new returns Err for anything larger.
                if let Ok(prefix) = IpPrefix::new(Ipv4Addr::from(addr), len.min(32)) {
                    table.insert(prefix, val);
                }
            }
            Op::Remove { addr, len } => {
                if let Ok(prefix) = IpPrefix::new(Ipv4Addr::from(addr), len.min(32)) {
                    let _ = table.remove(prefix);
                }
            }
            Op::LongestMatch { addr } => {
                let _ = table.longest_match(Ipv4Addr::from(addr));
            }
            Op::LongestMatchEntry { addr } => {
                let _ = table.longest_match_entry(Ipv4Addr::from(addr));
            }
            Op::Get { addr, len } => {
                if let Ok(prefix) = IpPrefix::new(Ipv4Addr::from(addr), len.min(32)) {
                    let _ = table.get(prefix);
                }
            }
            Op::Contains { addr, len } => {
                if let Ok(prefix) = IpPrefix::new(Ipv4Addr::from(addr), len.min(32)) {
                    let _ = table.contains(prefix);
                }
            }
            Op::Clear => {
                table.clear();
            }
            Op::Len => {
                let _ = table.len();
            }
            Op::IsEmpty => {
                let _ = table.is_empty();
            }
        }
    }
});
