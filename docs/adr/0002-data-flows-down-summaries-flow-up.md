# Data flows down; only summaries flow up

A child map is generated from its parent context: what the parent holds inside the child's area. After that, edits to the child never change the parent's data. The world map is mostly a political and regional map with flavour visuals; the geography lives in the region maps. A parent can show a summary that it derives from a child, for example the dominant biome of a region as a fill colour on the world map. A summary is computed and never edited.

We rejected two-way sync because it needs conflict rules for every layer at every scale.

## Consequences

- A child keeps a snapshot of its parent context. If the parent changes later, the child is flagged as out of date, and the user can re-sync it.
- Each map kind defines the summary it can give to its parent.
