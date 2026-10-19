# Entities are places; entity kinds are data

In v1 an entity kind exists only if it can be drawn on a map: political places (nation, region, settlement, landmark, dungeon) and natural features (mountain range, river, lake, forest, sea, island, desert). People, factions and events wait.

The map is what makes this tool different; a full worldbuilding suite (timelines, character sheets) is a second product. Entity kinds are rows in the database, described by field definitions, and relations are generic (from, to, type). So adding people or factions later adds data, not a schema change.
