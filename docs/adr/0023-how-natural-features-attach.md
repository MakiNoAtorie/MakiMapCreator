# How natural features attach to the map

Named rivers and lakes come from derived drainage, so they attach by anchor: a river keeps its mouth plus one user-picked point, a lake keeps one point inside it, all in root space. After each rebuild, the derived feature nearest the anchors takes the entity. If none lies within a tolerance (default 500 m), the entity becomes unplaced: it stays in the Outliner, flagged, with its page intact, and the user re-attaches it. Nothing is renamed or lost without the user seeing it.

Mountain ranges, forests, deserts and similar features are authored map objects (areas). The generator proposes outlines once; the user edits or draws them. We rejected detecting them from height or biome data on every rebuild, because their boundaries are a matter of taste and detection would move them under the user.
