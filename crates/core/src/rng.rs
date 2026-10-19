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
    // -0.0 and 0.0 are the same position.
    let bits = |v: f64| (v + 0.0).to_bits();
    let words = match key {
        Key::Entity(id) => [1, id as u64, 0],
        Key::At(p) => [2, bits(p.x), bits(p.y)],
    };
    let stage = stage.bytes().fold(0xCBF2_9CE4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01B3)
    });
    [stage].into_iter().chain(words).fold(mix(seed), |h, w| {
        mix(h ^ w.wrapping_add(0x9E37_79B9_7F4A_7C15))
    })
}

/// A uniform value in `0..1` for (seed, stage, key).
pub fn unit(seed: u64, stage: &str, key: Key) -> f64 {
    (hash(seed, stage, key) >> 11) as f64 / (1u64 << 53) as f64
}

/// Value noise in `0..1` on a root-space lattice of `cell` metres, the same at every map scale.
pub fn noise(seed: u64, stage: &str, at: RootPoint, cell: f64) -> f64 {
    let (gx, gy) = (at.x / cell, at.y / cell);
    let (x0, y0) = (gx.floor(), gy.floor());
    let corner = |i: f64, j: f64| {
        let p = RootPoint {
            x: (x0 + i) * cell,
            y: (y0 + j) * cell,
        };
        unit(seed, stage, Key::At(p))
    };
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (fx, fy) = (s(gx - x0), s(gy - y0));
    let top = corner(0.0, 0.0) + (corner(1.0, 0.0) - corner(0.0, 0.0)) * fx;
    let bottom = corner(0.0, 1.0) + (corner(1.0, 1.0) - corner(0.0, 1.0)) * fx;
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
