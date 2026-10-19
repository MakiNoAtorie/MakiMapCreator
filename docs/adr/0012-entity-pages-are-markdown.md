# Entity pages are Markdown with id links

Every entity has a page: Markdown text with sections, links to other entities, embedded images, and an infobox built from its fields. The user edits the source with a live preview. Links are stored by entity id with a display label, so renaming an entity does not break links; the editor completes links by name. The app shows backlinks ("mentioned in"). Each entity kind can have a page template of default sections. Images (map snapshots or the user's files) are stored in the project file.

We chose Markdown over a rich-text format because it is plain text that lasts, it exports to wikis and Obsidian with little work, and a rich-text editor in the UI toolkit is a large project. One renderer serves both the preview and the wiki export, so the preview is what the export gives.
