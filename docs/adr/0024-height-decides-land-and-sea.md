# Height decides land and sea

Height is the only authored terrain raster on every map kind. Land is height above the water level, sea is height at or below it. The water level is 0 m for every map for now (`mmc_core::WATER_LEVEL`). The world map's land and sea tools are height brushes: land raises the ground to +200 m, sea lowers it to -200 m, with the brush falloff and pressure, and neither tool moves ground the other way. A region generated from its parent reads the parent's height plus noise. The coast is where the parent's land and sea, taken as +200 m and -200 m (`COAST_RELIEF`), plus the same noise cross 0 m, so it stays within about one parent cell (ADR-0008) and flat parent sea makes no islands. Only ground on the wrong side of that coast moves, to 0.5 m across it; the rest keeps the parent's relief.

We rejected the separate land mask. With two layers the coast ignored the ground: raised ground could be sea, and land showed as a flat plateau.

## Consequences

- Files with the old `land` layer migrate once, on open, in core: land at or below 0 m goes to +200 m, sea at or above 0 m goes to -200 m, other heights stay. The land tiles and the land parent context are deleted, so a second open finds nothing to do.
- Lakes and a painted water level are later work; the derived land test is the only place that changes then.
