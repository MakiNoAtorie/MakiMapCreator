# Authored and derived layers

Every layer is either authored or derived. Authored layers are what the user edits and what the project file stores as truth: height, water level, biome paint, hand-drawn rivers, the lock mask. Derived layers are recomputed from authored layers after each edit and are never edited directly: drainage rivers, climate, automatic biomes. The user's paint sits on top of the derived result: biome paint overrides the automatic biome, and deleting an automatic river paints a suppression mask instead of removing data.

ADR-0003 (one-shot generation) applies to authored layers only. We chose live derived layers so that rivers and biomes follow terrain edits (mapgen4's main strength) and so that the stage pipeline knows exactly what to recompute.
