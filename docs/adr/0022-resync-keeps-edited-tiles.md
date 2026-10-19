# Re-sync and masked regeneration keep edited tiles

Each raster tile records whether the user ever edited it by hand. Re-syncing an out-of-date child map regenerates only tiles that were never edited, after a preview. A paintable lock mask, an authored layer, protects any further area. Masked regeneration (ADR-0003) respects both. The edited flag and the lock mask are Phase 0 seams.
