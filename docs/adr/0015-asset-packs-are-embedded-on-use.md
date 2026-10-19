# Asset packs are folders; projects embed the assets they use

Symbol and texture packs live as folders on disk, with one metadata file per pack. When a project uses an asset, the project file stores a copy of it, keyed by pack id, asset id and content hash. A project therefore opens on another machine without the pack. If the pack is present and has a newer version of the asset, the app offers to update it.

We rejected references only (projects break when moved) and embedding whole packs (files grow by unused assets).
