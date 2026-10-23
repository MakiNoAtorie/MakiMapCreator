# Live brush on the GPU

This refines ADR-0014 (brush stroke visible in one frame). While stroke parts are on their way through the edit worker, the canvas shows them from a GPU copy of the drawn heights, so the stroke is shaded under the pen in the frame its stamps are made, as in mapgen4 and Wonderdraft. The CPU stroke session stays the source of truth for undo, saving and derived layers; the copy is display only.

- The copy is the height raster of the last complete canvas render (the visible area at the drawn pyramid level, 2 pixels of margin), in an R32Float texture. Each render carries a mark: how many stamps of the current stroke were in the project when it read its tiles, and the stroke's flatten level (the edit worker writes it under the project lock). The copy is that raster plus every sent part the mark does not cover.
- Stamps run in compute passes (`render/src/live.wgsl`): one dispatch per part over the parts' box, each texel applying the part's stamps in order; smooth takes two dispatches per stamp (row means, then box means), because each stamp reads the heights the one before left. All modes of the CPU brush run there. Corners hash with a u32 port of splitmix64; core gives the per-axis hash states and smoothstep weights (`detail_tables`), because WGSL has no u64 or f64. Floats are f32.
- Flatten's level is the CPU's from the mark, or, for the part that starts the stroke, read from the copy under its first stamp.
- Shading is the height pass of the shown map's kind, built for the window format and drawn by an egui paint callback over the canvas image. Outside the copy the canvas image shows.
- The copy is uploaded at a stroke's first part and again when a complete render covers other pixels (pan, zoom). A complete render that covers the same pixels only retires the parts it shows. When no part is left the canvas image, which then has every stamp, shows alone.

We rejected a GPU copy that is the source of truth (read back for undo and save): it would make the result depend on the GPU and break seam-safe determinism (ADR-0008, ADR-0021).

## Reference maths

`mmc_core::brush` is the reference; a port changes with it and `render/tests/live.rs` holds them together.

- Pixels are global pixel indices `g = round(at / mpp - 0.5)` with centres `(g + 0.5) * mpp`, root-space metres.
- `stamp_weight = brush_weight(d², falloff) * max(0, 1 + amount * detail_noise)`, `d² = |at - centre|² / radius²`, 0 outside the disc; `amount` is clamped to 0..1, so detail never turns a stamp around.
- `detail_noise`: nodes every 4 global pixels at `g = 4k`, value `sum(0.5^o * (2 * rng::noise(seed, "brush-detail", node, scale / 2^o) - 1)) / 1.875` over octaves 0..4, skipping octaves whose cell is under 8 pixels. A pixel is the bilinear mix of its 4 nodes, weights `(g mod 4) / 4`, first in y, then in x, each `a + (b - a) * t`. `rng::noise` is smoothstep value noise on lattice corners hashed with splitmix64 from the corner's f64 bits.
- Modes, with `w` the weight and `p` the pressure: add `v += strength * p * w`; raise-to and lower-to `v += (target - v) * min(1, p * w)`, never the other way; smooth `v += (mean - v) * min(1, strength * p * w)`, `mean` the box mean of the `(2k + 1)²` pixels around inside the map, `k = max(1, ceil(radius px / 4))`, read before the stamp; flatten `v += (level - v) * min(1, strength * p * w)`, `level` the pixel under the stroke's first stamp before the stroke; roughen `v += strength * p * falloff * detail_noise`.
- A port with f32 or another hash agrees within its rounding, not bit for bit.

## Consequences

- At level 0 GPU and CPU agree within 1e-3 m (measured 2.5e-4 m at 800 m under WARP), the shaded colours within 1 per channel, so the hand-over shows no jump. Smooth's box stops at the copy's edge, where the CPU's goes on to the map's.
- At pyramid levels above 0 a texel is the mean of its level-0 pixels, as the CPU pyramid: a stamp changes it by the mean change of its pixels (all of them up to level 2, 4x4 of them above), each starting on the plane of the texel's slope. Add and roughen are linear, so they match within 1e-3 m; the other modes within 0.2 m (measured up to 0.11 m at level 2 on an 800 m hill), colours within 1 per channel off a 0 m coastline, which such a difference can move by a texel.
- A stroke past the copy's edge is drawn there by the canvas image when the CPU has applied it; the commit is the CPU's either way.
- A failed part, or a flatten part that starts off the copy, hides the copy until the next complete render.
