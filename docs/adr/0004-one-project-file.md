---
status: superseded by ADR-0011 (format only; one file per project still holds)
---

# One file per project

The whole map tree is saved in one project file. The file is chunked per map and per layer, so large child maps load only when the user opens them. The format is versioned with a chain of converters.

We rejected one file per map with links between them, because links between files break when the user moves, renames or shares files.
