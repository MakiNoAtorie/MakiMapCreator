# Features and Phase 0 seams (draft)

Status: working draft. Terms follow `GLOSSARY.md`. Decisions: `docs/adr/0001`–`0023`, UI rules in `docs/design/ui.md`, `doc-research/SYNTHESIS.md` §6.

## 1. Roadmap

Phase 0 (core, plus a crude world map to prove nesting) → v1 region map → v2 world map → v3 settlement map → dungeon map. See ADR-0006.

## 2. Features per map kind

Each kind has a generate mode and an edit mode, can be a root map, and defines what it reads from its parent context and the summary it gives back.

### Region map (v1)
- Generate (from parent context or from a template): height, sea and lakes, rivers, climate, biomes. Major rivers from the parent are path constraints.
- Edit: height brushes with pen pressure, water level, draw rivers, paint biomes, masked regeneration.
- Edges: bounded refinement of the parent coastline, root-space noise, ghost overlay of neighbour edges (ADR-0008).
- Layers: authored (height, water level, biome paint, hand-drawn rivers, lock mask) and derived, recomputed after each edit (drainage rivers, climate, automatic biomes) (ADR-0018).
- Entities: rivers and lakes attach to derived drainage by anchor; ranges, forests and deserts are authored areas the generator proposes once (ADR-0023). Settlement and landmark points.
- Labels: placed automatically for entities (collision avoidance, along river paths, spaced range names); a dragged offset is saved on the map object.
- Children: select a settlement point to create a settlement map.
- Summary to parent: dominant biome, refined coastline (display only).
- Explore later: an Erode brush that runs stream-power, hillslope and talus erosion on the GPU over the brushed area and saves it as one undoable edit (Schott et al. 2023; step 3 in `doc-research/terrain-mountains.md`). Worth it only if the erosion relief stage (#30) is not enough.
- Explore later: generate travel routes between settlements and points of interest as weighted shortest paths over a biome and terrain cost field; the most used paths become roads. References: Azgaar's Fantasy Map Generator (roads between burgs), Scientific Reports 2022 model of co-evolving cities and roads (PubMed 35710781).

### World map (v2)
- Generate: coarse geography (land and sea, elevation, climate zone, biome class, major rivers), then regions and nations over it (cost-based expansion).
- Edit: paint land and sea, move, merge or split region borders, rename, recolour.
- Shows: political fills, borders, labels, flavour symbols drawn from the coarse data and editable by hand, child summaries as tints.
- Children: select a region to create a region map.
- Parent context it gives: coarse layers inside the area plus a margin, major river paths, region outline.

### Settlement map (v3)
- Generate from parent context: terrain slope, rivers, coast, roads that enter, size class from the settlement's population field.
- Generates: walls, wards, streets, buildings (TownGeneratorOS ideas, clean-room code).
- Edit: move streets and buildings, re-zone wards, masked regeneration.
- Children: a building or point to create a dungeon map.

### Dungeon map (later)
- Rooms and corridors on a grid or room graph. Root map or child of any map.

### All map kinds
- Default map sizes, changeable at creation; a child's scale defaults to its parent's scale times a set factor:

  | Map kind | Size | Scale | Covers |
  |---|---|---|---|
  | World | 4096² px | 1 km/px | about 4 000 km |
  | Region | 8192² px | 10 m/px | about 82 km |
  | Settlement | 4096² px | 0.5 m/px | about 2 km |
- One project per app window; maps and pages open as tabs. A second project opens a second window (process); copy between projects goes through the clipboard.
- Deletion, all undoable:
  - Delete a map: its entities and pages stay; its map objects go; its child maps become root maps.
  - Delete an entity that has a map: the app asks whether to delete the map or keep it as a root map. Links to the entity show as missing; the link text stays.

## 3. Places and pages (all kinds, from v1)

- Entities: political places and natural features; kinds are data (ADR-0009).
- Fields: built-in plus user fields per project, including entity references; field sets as template files (ADR-0010).
- Relations: typed links between entities ("part of", "capital of").
- Pages (ADR-0012):
  - Markdown with inline live preview (Obsidian-like; editing core reused from Just-a-markdown-editor); sections from the kind's page template; add, remove and reorder sections.
  - Entity links stored by id, completed by name, survive renames; backlinks panel.
  - Infobox from fields; images (map snapshots, user files) stored in the project.
  - Open a page by clicking a place on a map; "show on map" from a page; entities with no map still have pages.
- Browser: list, search and filter entities by kind, field and tag.

## 4. Export

- Images (PNG and others) and PDF per map.
- Wiki export: a folder of Markdown pages, one per entity, with `[[links]]`, infoboxes and map images. Opens in Obsidian as-is. Uses the same renderer as the page preview. Offline HTML later.
- No cloud (ADR-0005).

## 5. Phase 0 seams

Build only what is expensive to add later. Each seam gets one real use in the proof slice.

| Seam | Why now |
|---|---|
| Project tree: maps, parent link, area on parent | Every kind and the file format depend on it |
| Root space in metres: per-map transform and metres per pixel; f32 height in metres (ADR-0020); noise sampled in root space | Neighbour seams and parent context depend on it (ADR-0008) |
| Generic layers: tiled raster, map objects with stable ids | Each kind stores different data; retrofitting storage touches everything |
| Map kind interface: generators, tools, layers, render passes, child creation, summary | Kinds are built independently behind it |
| Parent context: sample parent layers in an area plus margin, snapshot, out-of-date flag | Hard to add once kinds exist |
| Entities, relations, field definitions, pages (schema and core API) | Generators create entities; the schema must exist before the first save |
| Stage pipeline: stages declare read and write layers, dirty flags | v1 full rebuild still runs through it, so incremental work later does not change callers |
| Authored and derived layers; per-tile edited flag; lock mask (ADR-0018, ADR-0022) | The pipeline, save and re-sync all depend on the split |
| Ownership: each map owns only its outline; outside is read-only context (ADR-0019) | Tools and save must respect it from the first edit |
| Commands and undo: every edit is a command; tile deltas for rasters; one history per map and per page (ADR-0016) | Undo can not be added after tools exist |
| Jobs: background generation, latest request wins, double-buffered results | UI must not block; threading is hard to add later |
| Hash RNG keyed by (seed, stage, entity id or root-space position) | Determinism under parallel work and masked regeneration |
| Project file: SQLite working copy (WAL), background save by `VACUUM INTO`, migrations (ADR-0011) | Format v1 must be readable forever |
| Tile pyramid: per-layer levels, async load, LRU cache, stored map overview; user-set map size (default 8192²) | Large maps and fast open depend on it (ADR-0014) |
| Search: FTS5 index in the same transaction as each change; in-memory name index | Lookups must never wait (ADR-0014) |
| Embedded assets: table keyed by pack, asset and hash (ADR-0015) | Part of the file schema |
| Benchmark gate: fixtures and budgets from ADR-0014, run in CI | Budgets hold only if every change is measured |
| String lookup (Fluent) for all UI text (ADR-0017) | Retrofitting touches every widget |
| Workspace shell: panel and tool registry per kind and mode, pen and mouse input | Kinds plug UI in without touching the shell |
| Renderer: wgpu, layer draw passes from attribute buffers; draws to any target, tile by tile, with no window | Kinds add passes independently; export (PDF, large images) needs offscreen tiled rendering |

Not in Phase 0: real generators, rivers, biomes, political algorithms, styling, export, the page editor UI.

### Proof slice
1. Root world map: paint land and sea, generate crude regions as entities.
2. Select a region, create a child region map from parent context (upsample plus root-space noise).
3. Paint height on the child with pen pressure. Undo and redo.
4. World map shows the child's summary as a tint.
5. Give the region a page with one link to another entity; rename the target; the link still resolves.
6. Save while painting; close, reload; the tree, layers, entities and pages match what was saved.
7. Search a place name from the command search; results appear within the ADR-0014 budget on a fixture with 10 000 entities.
8. Edit the world inside the region's area; the child shows as out of date.
9. Export the region map offscreen to a PNG larger than the screen.

## 6. Open questions
- eframe or our own winit loop (ADR-0013): decided by the WinTab test in Phase 0.
