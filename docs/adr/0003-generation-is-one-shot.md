# Generation is one-shot; regeneration is masked

Generation fills a map's layers once. After that the layers are ordinary editable data, and nothing keeps them linked to the generator. To regenerate, the user selects an area or unlocks parts of the map, and the generator overwrites only those parts.

We rejected live generation under stored edits (edits kept as overlays that are replayed on each regeneration), because FMG's Keep/Risk/Erase shows how complex that gets. Masked regeneration covers the need for now.
