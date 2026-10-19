# A project is a tree of maps at different scales

A project holds a tree of maps. Each map has one kind (world, region, settlement, dungeon), its own layers, and an optional link to an area on its parent. The user drills down by selecting an area or point on a map and generating a child map there. Any kind can be the root, so a user can design only a region, a settlement or a dungeon without the levels above it.

We chose this over one map with zoom levels and over separate apps per scale. Each scale needs different data (raster terrain, vector streets, room graphs). Each scale also needs its own generators and tools. And children must stay consistent with their parent.

## Consequences

- Each map kind is a module built into the app (one crate per kind) behind one interface; there are no runtime or third-party plugins. The interface covers: generators, tools, layer types, render passes, and "make a child from parent context". Kinds are built and shipped independently.
- The core stores raster tiles and vector objects generically. Each kind picks the layers it uses.
- Workspaces adapt to the kind of the active map. Generate and Edit are modes inside a kind, not separate tabs.
