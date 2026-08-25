// Does a std HashMap's iteration order actually vary across runs, and at what
// key count? This is the question "what does the second futex buy" reduced to
// its mechanism: detcore's `blocked.futex_waiters` is
// HashMap<FutexID, Vec<FutexWaiter>> with the default RandomState hasher.
//
// A proxy for FutexID::Private{mm,address}: a two-integer key with a derived
// Hash. Stated as a proxy, not the real type -- what is being measured is
// RandomState's per-process seeding, which does not depend on the key type.
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Hash, PartialEq, Eq, Debug)]
struct Key { mm: u64, address: usize }

fn main() {
    for n in [1usize, 2, 3, 4, 8] {
        let mut m: HashMap<Key, u32> = HashMap::new();
        for i in 0..n { m.insert(Key { mm: 1, address: 0x7fff_0000 + i * 0x40 }, i as u32); }
        let order: Vec<usize> = m.keys().map(|k| k.address).collect();
        let mut h = DefaultHasher::new();
        order.hash(&mut h);
        println!("n={} order_digest={:016x}", n, h.finish());
    }
}
