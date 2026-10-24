//! Stateless hash RNG keyed by (seed, stage, key); stable within one app version (ADR-0021).

use crate::RootPoint;

/// What a random value belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Entity(i64),
    /// A root-space position in metres (ADR-0008).
    At(RootPoint),
}

fn mix(mut z: u64) -> u64 {
    // splitmix64 finaliser.
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform 64-bit value for (seed, stage, key).
pub fn hash(seed: u64, stage: &str, key: Key) -> u64 {
    hash_from(prefix(seed, stage), key)
}

fn step(h: u64, w: u64) -> u64 {
    mix(h ^ w.wrapping_add(GOLDEN))
}

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// The hash state after (seed, stage), so many keys of one stage hash the stage once.
pub(crate) fn prefix(seed: u64, stage: &str) -> u64 {
    let stage = stage.bytes().fold(0xCBF2_9CE4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01B3)
    });
    step(mix(seed), stage)
}

pub(crate) fn hash_from(prefix: u64, key: Key) -> u64 {
    match key {
        Key::Entity(id) => [1, id as u64, 0].into_iter().fold(prefix, step),
        Key::At(p) => at_finish(at_x(prefix, p.x), at_y(p.y)),
    }
}

// -0.0 and 0.0 are the same position.
fn bits(v: f64) -> u64 {
    (v + 0.0).to_bits()
}

/// The hash state of a `Key::At` with root-space `x`, before its `y`.
pub(crate) fn at_x(prefix: u64, x: f64) -> u64 {
    [2, bits(x)].into_iter().fold(prefix, step)
}

/// The word a `Key::At`'s `y` mixes in: `at_finish(at_x(prefix, x), at_y(y))` is its hash.
pub(crate) fn at_y(y: f64) -> u64 {
    bits(y).wrapping_add(GOLDEN)
}

pub(crate) fn at_finish(state: u64, word: u64) -> u64 {
    mix(state ^ word)
}

/// A hash of integer lattice corner (`ix`, `iy`) after `prefix`, each wrapped to 32 bits, so a
/// u32 port computes it.
pub(crate) fn lattice(prefix: u64, ix: i64, iy: i64) -> u64 {
    mix(prefix ^ (ix as u32 as u64 | (iy as u32 as u64) << 32))
}

/// A uniform value in `0..1` for (seed, stage, key).
pub fn unit(seed: u64, stage: &str, key: Key) -> f64 {
    to_unit(hash(seed, stage, key))
}

pub(crate) fn to_unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Value noise in `0..1` on a root-space lattice of `cell` metres, the same at every map scale.
pub fn noise(seed: u64, stage: &str, at: RootPoint, cell: f64) -> f64 {
    noise_with(at, cell, |cx, cy| {
        let p = RootPoint {
            x: cx * cell,
            y: cy * cell,
        };
        unit(seed, stage, Key::At(p))
    })
}

/// `noise` with the lattice value at integer lattice coordinates (`cx`, `cy`) from `corner`.
pub(crate) fn noise_with(at: RootPoint, cell: f64, corner: impl Fn(f64, f64) -> f64) -> f64 {
    let ((x0, fx), (y0, fy)) = (axis(at.x, cell), axis(at.y, cell));
    let corner = |i: f64, j: f64| corner(x0 + i, y0 + j);
    let c = [
        corner(0.0, 0.0),
        corner(1.0, 0.0),
        corner(0.0, 1.0),
        corner(1.0, 1.0),
    ];
    mix4(c, fx, fy)
}

/// Lattice coordinate below `v` and the smoothstep weight toward the next one.
pub(crate) fn axis(v: f64, cell: f64) -> (f64, f64) {
    let g = v / cell;
    let g0 = g.floor();
    let t = g - g0;
    (g0, t * t * (3.0 - 2.0 * t))
}

/// Corners `[x0y0, x1y0, x0y1, x1y1]` mixed in x, then y.
pub(crate) fn mix4([a, b, c, d]: [f64; 4], fx: f64, fy: f64) -> f64 {
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: RootPoint = RootPoint {
        x: 1234.5,
        y: -50.0,
    };

    #[test]
    fn hash_depends_on_seed_stage_and_key_only() {
        let h = hash(7, "rivers", Key::At(P));
        assert_eq!(h, hash(7, "rivers", Key::At(P)));
        assert_ne!(h, hash(8, "rivers", Key::At(P)));
        assert_ne!(h, hash(7, "biomes", Key::At(P)));
        assert_ne!(
            h,
            hash(
                7,
                "rivers",
                Key::At(RootPoint {
                    x: 1234.5,
                    y: -49.0
                })
            )
        );
        assert_ne!(hash(7, "s", Key::Entity(1)), hash(7, "s", Key::Entity(2)));
        let zero = RootPoint { x: 0.0, y: 0.0 };
        let neg = RootPoint { x: -0.0, y: 0.0 };
        assert_eq!(hash(1, "s", Key::At(zero)), hash(1, "s", Key::At(neg)));
    }

    #[test]
    fn unit_values_are_spread_over_0_to_1() {
        let v: Vec<f64> = (0..1000).map(|i| unit(3, "s", Key::Entity(i))).collect();
        assert!(v.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        assert!((mean - 0.5).abs() < 0.05, "mean {mean}");
    }

    #[test]
    fn noise_is_in_range_continuous_and_hits_lattice_values() {
        let n = |x, y| noise(5, "s", RootPoint { x, y }, 100.0);
        let corner = unit(5, "s", Key::At(RootPoint { x: 300.0, y: 200.0 }));
        assert_eq!(n(300.0, 200.0), corner);
        for i in 0..500 {
            let x = i as f64 * 3.7 - 900.0;
            let v = n(x, 42.0);
            assert!((0.0..1.0).contains(&v));
            assert!((v - n(x + 0.01, 42.0)).abs() < 0.01);
        }
    }
}
