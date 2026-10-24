# Erosion relief is a derived shading layer

This applies ADR-0018 to mountain relief. Gullies and ridges that run down the slope come from an erosion filter written from runevision's article "Fast and Gorgeous Erosion Filter" (2026); the MPL-2.0 `bevy_erosion_filter` was read as a reference only. The filter is a pure function of the authored height, the project seed and the root position, so it is a derived layer (`relief`, metres) that the region kind's height pass adds to height before shading. It never changes the stored height, the land test or the coastline, and it never compounds: rerunning it, or painting strokes in another order that gives the same height, gives the same relief.

- Reference: `mmc_core::relief`; GPU port: `render/src/relief.wgsl`. The stage (`mmc-kind-region`, `RELIEF`) runs after each edit like the other stages.
- Input slope: central differences of block means of the pyramid level `relief_level(mpp)` (blocks of about `RELIEF_BLOCK` = 160 m, at least level 1) on the global pixel grid, bilinear between block centres. Brush detail finer than a block does not steer the gullies.
- Masks: `smoothstep(COAST_BAND, height option, h - WATER_LEVEL)` times `ease_out(|slope| / slope option)`, `ease_out(t) = 1 - (1 - t)²`; flat, low and sea ground get none, so the coast never moves.
- Octaves: 4, cells `RELIEF_CELL / 2^o` (640 m down to 80 m), amplitude `0.5^o`, faded out below 4 pixels per cell (`lod`). Each octave is Phacelle-style noise: one splat per cell at a pivot hashed by `rng::lattice`, the 4 x 4 cells around a point, weight `(1 - d² / 2.25)³` (zero at the edge of reach), stripes `(cos, sin)(2π dot(v, across))` across the slope, the weighted mean's length doubled and capped at 1. The cosine is the gully height; the sine's soft sign (`s / sqrt(s² + 0.09)`) times `RELIEF_BEND` x the slope turns the next octave's slope, so gullies branch; the mask is multiplied by the next slope's mask. Output: `strength * masks * sum / 1.875`. cos and sin are a Taylor series on the nearest half cycle, so the port needs no trigonometric builtin.
- Options (`relief-strength`, `relief-slope`, `relief-height`) are declared by the kind and stored once per project (`kind_options`), so neighbouring child maps always agree. They are settings, not edits: no undo step; setting one rebuilds the relief of every map of the kind.
- Live brush (ADR-0025): the copy is shaded from a second texture, heights plus relief, which compute passes (block means, then relief) refresh around each part's box. A cached scene that draws a relief layer is complete only when no stroke is unwritten and no rebuild is uncollected (`Project::derived_pending`), so the copy stays up until the canvas image has the matching relief.

We rejected the filter at brush time: each stamp would read the gradient the stamp before it changed, so gullies would compound and depend on pen speed and stroke order. We rejected a hard sign of the sine (the article's straight gullies): in f32 it made single pixels differ by up to 0.07 m from the CPU.

## Consequences

- At level 0 the GPU relief equals the CPU stage within 0.01 m (measured up to 1.8e-3 m over 13 seeds under WARP), away from the copy's edge by `relief_margin` (3 blocks, 48 pixels at 10 m): there the copy clamps and the CPU reads on, so a stroke at the screen edge can change its gullies at the hand-over. Above level 0 the copy's relief is the filter on the level's texels, while the canvas shows the mean of level-0 relief; they differ in fine gullies.
- The stage recomputes every tile of the map after each edit; a large map shows the copy until it ends.
- Export reads the stored relief; it is current after `wait_for_jobs`.
- Drainage, when it comes, should read height plus relief, so rivers follow the gullies.
