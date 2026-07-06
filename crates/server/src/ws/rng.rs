//! The seeded deterministic RNG harness that makes a match replayable.
//!
//! The authoritative server — never the client — is the source of every random
//! draw a match needs (Cop Event resolutions, crash-table rolls). Those draws
//! must be a *pure function of the match seed and the order in which they are
//! consumed*, so that re-running the same command stream against the same seed
//! reproduces a byte-identical match (acceptance criterion: *match RNG uses the
//! seeded deterministic harness so replays are reproducible*).
//!
//! The generator is [SplitMix64] — a tiny, well-distributed, allocation-free
//! PRNG with no external dependency, which keeps this harness as portable as the
//! rules crate it feeds. It also tracks a monotonic *draw cursor*: the number of
//! values drawn so far. The cursor is snapshotted to Redis alongside the live
//! match ([`super::protocol::MatchSnapshot`]) so a match resumed in another
//! process ([`SeededRng::resume`]) continues the *same* deterministic stream
//! rather than re-drawing from the top.
//!
//! [SplitMix64]: https://prng.di.unimi.it/splitmix64.c

use game_session::COP_EVENT_DIE_SIDES;

/// The SplitMix64 increment (the 64-bit golden-ratio constant).
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// A deterministic, seed-driven random source for one match.
///
/// Two generators built from the same seed and advanced the same number of
/// times produce identical draws — that is what lets a sealed replay reproduce
/// its match. `draws` is the cursor persisted so a resumed match continues the
/// stream instead of restarting it.
#[derive(Debug, Clone)]
pub struct SeededRng {
    /// The seed the match was opened with; retained so the stream can be
    /// re-derived (and audited) from the snapshot.
    seed: u64,
    /// The evolving SplitMix64 state.
    state: u64,
    /// How many values have been drawn — the cursor persisted to Redis.
    draws: u64,
}

impl SeededRng {
    /// A fresh generator seeded with the match's `rng_seed`, at draw cursor 0.
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            state: seed,
            draws: 0,
        }
    }

    /// Rebuild a generator for a resumed match: seed it, then fast-forward it by
    /// `draws` values so it continues exactly where the snapshot left off. This
    /// is what keeps a disconnect/reconnect (possibly in a different process)
    /// from corrupting the deterministic stream.
    pub fn resume(seed: u64, draws: u64) -> Self {
        let mut rng = Self::new(seed);
        for _ in 0..draws {
            rng.next_u64();
        }
        rng
    }

    /// The seed this generator was built from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The current draw cursor — persist this to resume the stream later.
    pub fn draws(&self) -> u64 {
        self.draws
    }

    /// Draw the next raw 64-bit value and advance the cursor (SplitMix64).
    fn next_u64(&mut self) -> u64 {
        self.draws += 1;
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Roll a `sides`-faced die, yielding a face in `1..=sides`. `sides` of 0 is
    /// meaningless and clamped to a single-faced die so a roll always returns a
    /// valid face rather than dividing by zero.
    pub fn roll(&mut self, sides: u8) -> u8 {
        let sides = sides.max(1) as u64;
        (self.next_u64() % sides) as u8 + 1
    }

    /// Draw the next seeded Cop Event face — a d10 in `1..=`[`COP_EVENT_DIE_SIDES`],
    /// exactly the `rngDraw` the `ResolveCopEventCmd` validates. The server draws
    /// this itself so a client can never bias the Cop Event table.
    pub fn next_cop_event(&mut self) -> u8 {
        self.roll(COP_EVENT_DIE_SIDES)
    }

    /// The face [`next_cop_event`](Self::next_cop_event) *would* return, computed
    /// without advancing the cursor. The server peeks the draw to feed the rules,
    /// then commits it only if the Cop Event resolution is accepted — a rejected
    /// attempt must not perturb the deterministic stream.
    pub fn peek_cop_event(&self) -> u8 {
        self.clone().next_cop_event()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_the_same_stream() {
        let mut a = SeededRng::new(0xDEAD_BEEF);
        let mut b = SeededRng::new(0xDEAD_BEEF);
        let seq_a: Vec<u8> = (0..16).map(|_| a.next_cop_event()).collect();
        let seq_b: Vec<u8> = (0..16).map(|_| b.next_cop_event()).collect();
        assert_eq!(seq_a, seq_b, "a seeded stream must be reproducible");
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SeededRng::new(1);
        let mut b = SeededRng::new(2);
        let seq_a: Vec<u8> = (0..16).map(|_| a.next_cop_event()).collect();
        let seq_b: Vec<u8> = (0..16).map(|_| b.next_cop_event()).collect();
        assert_ne!(
            seq_a, seq_b,
            "distinct seeds should not collide over 16 draws"
        );
    }

    #[test]
    fn cop_event_draws_are_valid_d10_faces() {
        let mut rng = SeededRng::new(42);
        for _ in 0..1_000 {
            let face = rng.next_cop_event();
            assert!(
                (1..=COP_EVENT_DIE_SIDES).contains(&face),
                "d10 face {face} out of range"
            );
        }
    }

    #[test]
    fn resume_continues_the_stream_without_replaying_it() {
        // Draw a full stream from a fresh generator.
        let mut full = SeededRng::new(7);
        let expected: Vec<u8> = (0..10).map(|_| full.next_cop_event()).collect();

        // Draw the first four, snapshot the cursor, then resume from it.
        let mut live = SeededRng::new(7);
        let first: Vec<u8> = (0..4).map(|_| live.next_cop_event()).collect();
        let mut resumed = SeededRng::resume(live.seed(), live.draws());
        let rest: Vec<u8> = (0..6).map(|_| resumed.next_cop_event()).collect();

        let stitched: Vec<u8> = first.into_iter().chain(rest).collect();
        assert_eq!(
            stitched, expected,
            "resuming from the cursor must continue, not restart, the stream"
        );
    }

    #[test]
    fn roll_clamps_a_zero_sided_die_instead_of_dividing_by_zero() {
        let mut rng = SeededRng::new(99);
        assert_eq!(rng.roll(0), 1, "a 0-sided die degrades to a single face");
    }
}
