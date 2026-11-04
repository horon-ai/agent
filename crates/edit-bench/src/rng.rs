//! Deterministic pseudo-random number generator.
//!
//! Fixture generation must be reproducible from `--seed` alone, so the crate
//! carries its own SplitMix64 instead of pulling a RNG dependency whose
//! stream could change between releases.

/// SplitMix64: 64-bit state, fixed output stream for a given seed.
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish value in `0..n`. `n == 0` yields `0`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

/// Fisher-Yates shuffle driven by `rng`.
pub(crate) fn shuffle<T>(items: &mut [T], rng: &mut SplitMix64) {
    for i in (1..items.len()).rev() {
        let j = rng.below(i + 1);
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(1);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = SplitMix64::new(7);
        for n in 1..40usize {
            for _ in 0..20 {
                assert!(rng.below(n) < n);
            }
        }
        assert_eq!(rng.below(0), 0);
    }
}
