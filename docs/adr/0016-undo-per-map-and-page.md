# Undo history per map and per page

Each open map has its own undo history, and each page has its own while it is edited. Undo in one tab never changes another map. An entity change (rename, field edit, placement) belongs to the history of the map or page where the user made it.

We rejected one project-wide history, because with several maps open, undo would change a map the user is not looking at. The cost: an entity change made on one map can be undone only from that map.
