// Binary trie (arena-based) baseline — recorded before Phase 6 (treebitmap rewrite).
// Run:          cargo bench
// HTML reports: target/criterion/report/index.html

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use routemap::RouteMap;
use ipnetx::prefix::IpPrefix;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::net::{Ipv4Addr, Ipv6Addr};

const SEED: u64 = 0xc0ffee;
const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

// Prefix length distributions are weighted toward the /16–/24 and /48–/64 ranges
// to reflect real-world BGP table shapes rather than uniform random lengths.

fn ipv4_prefixes(n: usize) -> Vec<IpPrefix<Ipv4Addr>> {
    let mut rng = StdRng::seed_from_u64(SEED);
    (0..n)
        .map(|_| {
            let addr = Ipv4Addr::from(rng.r#gen::<u32>());
            let len: u8 = match rng.r#gen_range(0..4u8) {
                0 => rng.r#gen_range(8..16),
                1 => rng.r#gen_range(16..20),
                2 => rng.r#gen_range(20..24),
                _ => rng.r#gen_range(24..29),
            };
            format!("{}/{}", addr, len).parse().unwrap()
        })
        .collect()
}

fn ipv4_addrs(n: usize) -> Vec<Ipv4Addr> {
    let mut rng = StdRng::seed_from_u64(SEED + 1);
    (0..n).map(|_| Ipv4Addr::from(rng.r#gen::<u32>())).collect()
}

fn ipv6_prefixes(n: usize) -> Vec<IpPrefix<Ipv6Addr>> {
    let mut rng = StdRng::seed_from_u64(SEED + 2);
    (0..n)
        .map(|_| {
            let addr = Ipv6Addr::from(rng.r#gen::<u128>());
            let len: u8 = match rng.r#gen_range(0..4u8) {
                0 => rng.r#gen_range(32..48),
                1 => rng.r#gen_range(48..56),
                2 => rng.r#gen_range(56..64),
                _ => 64,
            };
            format!("{}/{}", addr, len).parse().unwrap()
        })
        .collect()
}

fn ipv6_addrs(n: usize) -> Vec<Ipv6Addr> {
    let mut rng = StdRng::seed_from_u64(SEED + 3);
    (0..n)
        .map(|_| Ipv6Addr::from(rng.r#gen::<u128>()))
        .collect()
}

// ── insert ───────────────────────────────────────────────────────────────────
// Measures the time to build a fresh table from N prefixes.
// Each sample gets a fresh clone of the prefix list so insert costs are not
// amortized across iterations.

fn bench_insert_ipv4(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert/ipv4");
    for &size in &SIZES {
        let prefixes = ipv4_prefixes(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &prefixes,
            |b, prefixes| {
                b.iter_batched(
                    || prefixes.clone(),
                    |prefixes| {
                        let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
                        for (i, prefix) in prefixes.into_iter().enumerate() {
                            table.insert(prefix, i as u32);
                        }
                        table
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_insert_ipv6(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert/ipv6");
    for &size in &SIZES {
        let prefixes = ipv6_prefixes(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &prefixes,
            |b, prefixes| {
                b.iter_batched(
                    || prefixes.clone(),
                    |prefixes| {
                        let mut table: RouteMap<Ipv6Addr, u32> = RouteMap::new();
                        for (i, prefix) in prefixes.into_iter().enumerate() {
                            table.insert(prefix, i as u32);
                        }
                        table
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ── lookup ───────────────────────────────────────────────────────────────────
// Measures steady-state longest_match throughput on a pre-built table.
// The table is built once per size outside the timing loop.
// Lookup addresses use a different seed than the inserted prefixes to produce a
// realistic mix of hits and misses.

fn bench_lookup_ipv4(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup/ipv4");
    for &size in &SIZES {
        let mut table: RouteMap<Ipv4Addr, u32> = RouteMap::new();
        for (i, prefix) in ipv4_prefixes(size).into_iter().enumerate() {
            table.insert(prefix, i as u32);
        }
        let addrs = ipv4_addrs(size);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &addrs,
            |b, addrs| {
                b.iter(|| addrs.iter().map(|&addr| table.longest_match(addr)).count());
            },
        );
    }
    group.finish();
}

fn bench_lookup_ipv6(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup/ipv6");
    for &size in &SIZES {
        let mut table: RouteMap<Ipv6Addr, u32> = RouteMap::new();
        for (i, prefix) in ipv6_prefixes(size).into_iter().enumerate() {
            table.insert(prefix, i as u32);
        }
        let addrs = ipv6_addrs(size);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &addrs,
            |b, addrs| {
                b.iter(|| addrs.iter().map(|&addr| table.longest_match(addr)).count());
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert_ipv4,
    bench_insert_ipv6,
    bench_lookup_ipv4,
    bench_lookup_ipv6,
);
criterion_main!(benches);
