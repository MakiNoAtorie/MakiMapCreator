# Region edges: bounded refinement with root-space noise

A region map refines its parent's coastline and terrain, but the detail it adds can move a coastline only a bounded distance (about one parent cell). All procedural noise is sampled in root space, the coordinates of the root map, and never in a map's local coordinates. Parent context includes a margin outside the child's area.

The goal is that neighbouring regions match each other, not only the world map. Two regions that cover the same strip produce the same detail there, so seams match with no stitching code. An exact copy of the coarse parent coastline looks blocky, and free refinement breaks neighbours.

## Consequences

- Every map has a transform to root space. This is a Phase 0 seam.
- Hand edits near an edge can still diverge from a neighbour. The editor shows the neighbour's edge as a ghost overlay.
