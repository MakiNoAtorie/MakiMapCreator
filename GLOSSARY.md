# MakiMapCreator

A desktop tool to generate and then hand-edit fantasy maps at several scales, from a political world map down to settlements and dungeons.

## Language

### Structure

**Project**:
The unit the user opens and saves: one file that holds a tree of maps.
_Avoid_: Document, world (as a name for the whole file)

**Map**:
One node in the project tree, at one scale, with one map kind and its own layers.
_Avoid_: Level, sheet, canvas

**Map kind**:
The type of a map: world, region, settlement or dungeon. Each kind is a module built into the app that sets the generators, tools and layers the map can use.
_Avoid_: Map type, mode

**World map**:
The top-scale map. It is mostly political and regional, with flavour visuals; its geography is coarse.
_Avoid_: Overworld

**Region map**:
A map that holds the real geography (terrain, water, rivers, biomes) for one area of the world map or as a root.
_Avoid_: Zone, area map

**Settlement map**:
The plan of one town or city: streets, wards and buildings.
_Avoid_: Town map, city map

**Dungeon map**:
An interior map of rooms and corridors, as a child of any map or as a root.

**Child map**:
A map created on an area or point of its parent map.
_Avoid_: Submap, zoom map

**Outline**:
The area a child map owns, as a shape on its parent. Outside the outline the child map shows read-only context.
_Avoid_: Bounds, extent

**Root map**:
A map with no parent. Any map kind can be a root map.

**Root space**:
The coordinates of a map's root map, in metres. Every map has a transform to it, and procedural noise is always sampled in it.
_Avoid_: World space, global coordinates

### Flow between maps

**Parent context**:
The parent's data inside a child map's area, sampled when the child is generated. The child keeps a snapshot of it.
_Avoid_: Inherited data

**Summary**:
A value a child map derives for its parent to display, for example a region's dominant biome as a colour on the world map. It is computed, never edited.
_Avoid_: Rollup, sync

**Out of date**:
The state of a child map whose parent changed inside its area after the parent context snapshot was taken.
_Avoid_: Stale, dirty (dirty is for recompute)

### Working on a map

**Generate mode**:
The mode where the user sets parameters and a seed and runs generators on a map.

**Edit mode**:
The mode where the user changes a map's layers by hand with tools such as brushes.

**Authored layer**:
A layer the user edits and the project stores as truth, such as height or biome paint.

**Derived layer**:
A layer recomputed from authored layers after each edit, such as drainage rivers or climate. It is never edited directly.
_Avoid_: Generated layer (generation also writes authored layers)

**Relief**:
The derived layer of erosion gullies and ridges that the height shading adds to height. It never changes the stored height or the land test.
_Avoid_: Erosion (no material moves), detail (that is the brush noise)

**Water level**:
The height that splits land (above) from sea (at or below). It is 0 m on every map for now; there is no separate land layer.
_Avoid_: Sea mask, land layer

**Live copy**:
A GPU copy of the shown heights that a brush stroke changes in the frame it is drawn, until the stored result replaces it. It is never saved.

**Lock mask**:
An authored layer that marks areas that masked regeneration and re-sync must not change.

**Edited tile**:
A tile the user changed by hand at least once. Re-sync never regenerates it.

**Re-sync**:
Regenerating the unedited, unlocked tiles of an out-of-date child map from its new parent context.

**Masked regeneration**:
Running a generator again only inside a selection or the unlocked parts of a map.
_Avoid_: Reroll

**Layer**:
One stored set of data on a map: a raster in tiles (for example height) or a set of vector objects (for example rivers or streets).

**Workspace**:
The UI layout of panels and tools for the active map's kind and mode.
_Avoid_: Tab, screen

### Places and lore

**Entity**:
A named place in the project, such as a nation, settlement or river. It can appear on several maps or on none.
_Avoid_: Object, item, location

**Entity kind**:
The type of an entity, stored as data: a political place or a natural feature in v1.
_Avoid_: Category, class

**Natural feature**:
An entity for a named part of the geography, such as a mountain range, river, lake or forest.
_Avoid_: Landmark (a landmark is a built or notable place)

**Map object**:
The geometry (point, polygon or path) of an entity, or of an unnamed shape, on one map.
_Avoid_: Marker, pin

**Anchor**:
The root-space points that tie a named river or lake to the derived feature that currently matches it.
_Avoid_: Pin, handle

**Unplaced entity**:
An entity whose anchor matches no derived feature after a rebuild. It keeps its page and waits for the user to re-attach it.
_Avoid_: Orphan, lost entity

**Relation**:
A typed link from one entity to another, for example "part of" or "capital of".

**Field definition**:
The name, type and entity kind of one field. A field is built-in (the app uses it) or a user field (added per project).
_Avoid_: Attribute, property

**Page**:
The Markdown text of an entity, with sections, entity links, images and an infobox from its fields.
_Avoid_: Description, notes, article

**Page template**:
The default sections that a new page of an entity kind starts with.

### Assets

**Asset pack**:
A folder of symbols or textures with one metadata file, installed on the user's machine.
_Avoid_: Library, bundle

**Asset**:
One symbol or texture from an asset pack. A project stores a copy of each asset it uses.
_Avoid_: Stamp, sprite
