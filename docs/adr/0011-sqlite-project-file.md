# The project file is a SQLite database

Supersedes the file format in ADR-0004; one file per project still holds.

The project file is a SQLite database (rusqlite, bundled). The app edits a working copy in local app data. Save writes the working copy to the user's file with `VACUUM INTO` a temporary file and an atomic rename. Each raster tile is one zstd-compressed blob. `PRAGMA application_id` identifies the file; `PRAGMA user_version` plus ordered migrations handle versions, and an older app refuses a newer file. Only the persistence module uses SQL; generators and tools work on in-memory data. Undo history is not saved in v1.

The working copy keeps normal Save behaviour, puts no journal files next to the project, and survives a crash for restore. We rejected a custom chunked format (we would rebuild transactions, indexes and crash safety) and a zip archive (each save rewrites it, and it can not be queried).
