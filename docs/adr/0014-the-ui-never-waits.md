# The UI never waits; performance budgets are tested

The UI thread never blocks on disk, database or computation. Every query, load, save and generation runs on a worker; the UI shows the last result or a lower-resolution stand-in until the new one arrives, and a newer request cancels or replaces an older one.

Map size is set by the user per map; the default region map is 8192 × 8192 height pixels. Target hardware is current: a GPU with DX12 or Vulkan through wgpu, 64-bit Windows 10/11. No support for older hardware or software.

## Budgets

| Action | Budget |
|---|---|
| Brush stroke visible | 1 frame (16 ms) |
| Name lookup, link completion, command search (per keystroke) | 16 ms |
| Full-text search over all pages and fields | 50 ms for first results |
| Pan or zoom to unloaded area | stand-in in 1 frame, full tiles in 100 ms |
| Open project to first map frame | 1 s, any project size |
| Full rebuild of derived region layers at 4096² | 1 s, in background |
| Save | runs in background; the user keeps working |

## How

- Search: SQLite FTS5 index over entity names, pages and text fields, updated in the same transaction as each change. An in-memory name index (prefix and fuzzy) serves keystroke lookups without touching SQL.
- Tiles: each raster layer is stored as a pyramid of tiles (full resolution plus lower levels). Only visible tiles at the needed level load; an LRU cache bounds memory. The project file keeps a small overview of each map so it opens at once.
- Save: a background connection takes a read snapshot of the working copy (WAL mode) and writes it with `VACUUM INTO`, so edits continue during the save.
- Gate: a benchmark binary measures each budget on generated fixtures (large maps, 10 000+ entities, long pages); CI fails when a budget is exceeded. This follows the performance gate in Just-a-markdown-editor.
