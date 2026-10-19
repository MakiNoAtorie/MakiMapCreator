# Re-sync and masked regeneration keep edited tiles

Each raster tile records whether the user ever edited it by hand. Re-syncing an out-of-date child map regenerates only tiles that were never edited, after a preview. A paintable lock mask, an authored layer, protects any further area. Masked regeneration (ADR-0003) respects both. When a parent outline moves, the child map is out of date. On re-sync, ground the child gains is generated like unedited tiles; ground it loses becomes read-only context, and its data is deleted only when the user confirms. The preview shows any edited tiles on lost ground before deletion.

The edited flag and the lock mask are Phase 0 seams.
