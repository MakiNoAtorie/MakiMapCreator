# World geography is coarse data, drawn as symbols

The world map stores real geography at low resolution: land and sea, elevation, climate zone, a rough biome class, and major rivers as paths. Mountain, forest and other flavour symbols are drawn from this data; the user can also place, move or delete symbols by hand.

We rejected pure decoration. Without shared data, neighbouring regions invent different mountains and rivers, and their borders do not match. A major river must leave one region and enter the next at the same point, so its path is a constraint for both region generators. The political generator also needs terrain costs, and summaries need a world value to compare against.
