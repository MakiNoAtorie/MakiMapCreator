# Entity pages are Markdown with id links

Every entity has a page: Markdown text with sections, links to other entities, embedded images, and an infobox built from its fields. The editor uses inline live preview, as in Obsidian: text shows formatted, and the line with the cursor shows its Markdown source. Links are stored by entity id with a display label, so renaming an entity does not break links; the editor completes links by name. The app shows backlinks ("mentioned in"). Each entity kind can have a page template of default sections. Images (map snapshots or the user's files) are stored in the project file.

We chose Markdown over a rich-text format because it is plain text that lasts, it exports to wikis and Obsidian with little work, and a rich-text editor is a large project. One renderer serves both the preview and the wiki export, so the preview is what the export gives.

## Consequences

- The editing core comes from the owner's MIT project Just-a-markdown-editor (`github.com/MakiNoAtorie/Just-a-markdown-editor`): `src/core/buffer.rs` (rope document, operation undo, list-aware editing) and `src/core/markdown.rs` (pulldown-cmark presentation spans, block index, `link_at`, `format_at`). Both are UI-independent. Its winit, softbuffer and cosmic-text rendering is not reused; a new egui widget draws the presentation spans and maps input to document operations.
- The parser needs one addition: `[[id|label]]` entity links.
- While a page is focused, the page editor keeps its own undo. Each group of page edits enters the project command log as one command, so project undo and save stay consistent.
