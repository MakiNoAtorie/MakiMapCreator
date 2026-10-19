# Built-in fields plus user fields

Each entity kind has built-in fields that the app and generators read and write (for example settlement population, nation colour and capital). Users can add their own fields per project, with types text, Markdown, number, yes/no, choice and entity reference. One field definition table describes both; each entity stores its values in one JSON column, which the app checks against the definitions.

We rejected a fixed field set, because every world needs different fields, and one table row per value, because a JSON column is simpler and SQLite can query it. Field definitions drive the infobox on pages and in the wiki export, so no kind needs its own export code. Field sets can be saved to and loaded from small template files.
