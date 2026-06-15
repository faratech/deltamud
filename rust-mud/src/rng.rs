// Deterministic PRNG — a 1:1 port of CircleMUD's random.c (Park-Miller /
// Lehmer minimal-standard generator) plus number()/dice() from utils.c.
//
// Owning the generator in GameState (instead of rand::thread_rng) makes the
// game reproducible: pinning the seed lets a scripted session produce the
// exact same dice/skill rolls as the C MUD, which is the basis of the
// dual-MUD byte-parity golden tests.

const M: i64 = 2147483647;
const Q: i64 = 127773;
const A: i64 = 16807;
const R: i64 = 2836;

#[derive(Debug, Clone)]
pub struct Rng {
    seed: i64,
}

impl Rng {
    pub fn new(initial_seed: u64) -> Self {
        // Keep the seed in (0, M) like circle_srandom expects.
        let s = (initial_seed % (M as u64)).max(1) as i64;
        Rng { seed: s }
    }

    pub fn srandom(&mut self, initial_seed: u64) {
        self.seed = (initial_seed % (M as u64)).max(1) as i64;
    }

    /// circle_random(): one step of the Lehmer generator.
    pub fn circle_random(&mut self) -> i64 {
        let hi = self.seed / Q;
        let lo = self.seed % Q;
        let test = A * lo - R * hi;
        self.seed = if test > 0 { test } else { test + M };
        self.seed
    }

    /// number(from, to): inclusive uniform integer, matching utils.c.
    pub fn number(&mut self, from: i32, to: i32) -> i32 {
        let (from, to) = if from > to { (to, from) } else { (from, to) };
        let span = (to - from + 1) as i64;
        (self.circle_random() % span) as i32 + from
    }

    /// dice(num, size): sum of `num` rolls of a `size`-sided die.
    pub fn dice(&mut self, num: i32, size: i32) -> i32 {
        if size <= 0 || num <= 0 {
            return 0;
        }
        let mut sum = 0;
        for _ in 0..num {
            sum += (self.circle_random() % size as i64) as i32 + 1;
        }
        sum
    }
}

impl Default for Rng {
    fn default() -> Self {
        // Fixed default seed; the boot path reseeds from the clock unless a
        // pinned seed is requested (golden tests).
        Rng::new(1)
    }
}
