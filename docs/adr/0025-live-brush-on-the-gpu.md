# Live brush on the GPU

This refines ADR-0014 (brush stroke visible in one frame). While stroke parts are on their way through the edit worker, the canvas shows them from a GPU copy of the drawn heights, so the stroke is shaded under the pen in the frame its stamps are made, as in mapgen4 and Wonderdraft. The CPU stroke session stays the source of truth for undo, saving and derived layers; the copy is display only.

- The copy is the height raster of the last complete canvas render (the visible area at the drawn pyramid level, 2 pixels of margin), in an R32Float texture. Each render carries how many stamps of the current stroke were in the project when it read its tiles (the edit worker writes this mark under the project lock). The copy is that raster plus every sent part the mark does not cover.
- Stamps run in a compute pass (`render/src/live.wgsl`), one dispatch per part over the parts' box; each texel applies the part's stamps in order. The maths is `stamp_weight`, `detail_noise` and the add, target and roughen modes of the CPU brush. Corners hash with a u32 port of splitmix64; core gives the per-axis hash states and smoothstep weights (`detail_tables`), because WGSL has no u64 or f64. Floats are f32.
- Shading is the kind's height pass, built for the window format and drawn by an egui paint callback over the canvas image. Outside the copy the canvas image shows.
- The copy is uploaded at a stroke's first part and again when a complete render covers other pixels (pan, zoom). A complete render that covers the same pixels only retires the parts it shows. When no part is left the canvas image, which then has every stamp, shows alone.
- Smooth and flatten stay on the CPU path with the disc overlay: they read neighbours or the stroke start.

We rejected a GPU copy that is the source of truth (read back for undo and save): it would make the result depend on the GPU and break seam-safe determinism (ADR-0008, ADR-0021).

## Consequences

- GPU and CPU agree within 1e-3 m (measured 2.5e-4 m at 800 m under WARP); the shaded colours within 1 per channel, so the hand-over shows no jump. At pyramid levels above 0 a texel is the mean of its level-0 pixels, as the CPU pyramid: a stamp changes it by the mean change of its pixels (all of them up to level 2, 4x4 of them above), each starting on the plane of the texel's slope. Add and roughen are linear, so they match within 1e-3 m; raise-to and lower-to within 0.1 m (measured 0.08 m at level 2 on an 800 m hill), colours within 1 per channel.
- A stroke past the copy's edge is drawn there by the canvas image when the CPU has applied it; the commit is the CPU's either way.
- A failed part, or a part the GPU cannot do, hides the copy until the next complete render.
