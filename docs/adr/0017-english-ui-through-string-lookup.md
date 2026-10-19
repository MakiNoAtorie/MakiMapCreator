# English UI through a string lookup

The UI ships in English only. Every UI string still goes through a lookup (Fluent), never a literal in widget code, so translations can be added later without touching the UI code. Project content (names, pages) is Unicode and is never translated.
