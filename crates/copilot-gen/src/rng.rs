//! A small deterministic generator.
//!
//! `splitmix64`, chosen so that generation depends on nothing but the seed. A
//! failing case is then reproducible from the seed alone, which is what makes a
//! randomised differential test usable rather than merely alarming.

/// A seeded pseudo-random source.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator with the given seed.
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. Returns zero when `bound` is zero.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// A value in `low..=high`.
    pub fn between(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    /// A coin flip.
    pub fn flip(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// One element of a slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
