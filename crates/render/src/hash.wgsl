// u64 arithmetic in u32 pairs (low, high): the splitmix64 finaliser of `rng::mix`.

fn mul_wide(a: u32, b: u32) -> vec2u {
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    return vec2u((p00 & 0xffffu) | (mid << 16u), a1 * b1 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u));
}

fn mul64(a: vec2u, b: vec2u) -> vec2u {
    let w = mul_wide(a.x, b.x);
    return vec2u(w.x, w.y + a.x * b.y + a.y * b.x);
}

fn shr64(a: vec2u, s: u32) -> vec2u {
    return vec2u((a.x >> s) | (a.y << (32u - s)), a.y >> s);
}

// splitmix64 finaliser, as `rng::mix`.
fn mix64(z0: vec2u) -> vec2u {
    var z = mul64(z0 ^ shr64(z0, 30u), vec2u(0x1CE4E5B9u, 0xBF58476Du));
    z = mul64(z ^ shr64(z, 27u), vec2u(0x133111EBu, 0x94D049BBu));
    return z ^ shr64(z, 31u);
}
