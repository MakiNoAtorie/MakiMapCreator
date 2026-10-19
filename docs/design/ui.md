# UI rules

Status: binding for all UI work. Reference mockup: `spikes/ui-proto` (not in git; screenshots in `spikes/ui-proto/shots`). Toolkit: ADR-0013.

Goal: a functional creative tool in the family of Blender, Krita and Affinity. Not the generic "AI tool" look.

## Do not use
- Rounded cards, soft shadows on panels, glass blur, gradients.
- Indigo or purple accent on dark slate (the shadcn/Tailwind default).
- Emoji as icons, pill buttons, centred hero empty states, illustrations.
- Large padding, card grids for settings, Inter at 14–16 px for all text.

## Layout
- The map canvas takes all free space. Panels dock and collapse; Tab hides all panels.
- Top: menu bar with breadcrumb (root › … › active map), map kind, and the Edit/Generate mode switch on the right.
- Below it: the tool options bar for the active tool or generator (size, strength, falloff, pen pressure toggles; seed and regenerate in Generate mode).
- Left: tool strip in Edit mode (32 px buttons, drawn line icons, tooltip with name and shortcut); generator parameters and pipeline status in Generate mode.
- Right: Outliner (entity tree, kind right-aligned), Properties (fields grid, entity links), Layers.
- Bottom: status bar with cursor position, elevation, biome, entity under cursor; map size, scale and dirty tiles on the right.
- Centre: tabs for open maps and pages ("Map: Northlands", "Page: Riverford").

## Style
- Controls 22–24 px high. Corner radius 2 px maximum. Borders 1 px. Shadows only on popups and menus.
- Spacing: item 6×4 px, button padding 6×2 px.
- Number fields drag to change; sliders show their value.
- Section headers: small capitals in the weak text colour, no boxes.

## Colour
- Warm neutral greys (paper and ink), light and dark themes.
- One muted accent (ochre in dark, deep ochre in light), used only for selection, the active tool, the active mode and entity links.
- Map colours never take the UI tint. The map has its own cartographic palette.

## Type
- UI: neutral sans, 13 px body, 11 px small, tabular figures for numbers. Bundle IBM Plex Sans or Source Sans 3 (the mockup used Segoe UI).
- Pages and map labels: a serif. Bundle Literata or Source Serif 4 (the mockup used Georgia). Page body about 15.5 px with 24 px line height; settlement labels bold serif; rivers italic serif; ranges and regions letter-spaced capitals.
- Bundle only fonts with licenses that fit MIT distribution (SIL OFL is fine).

## Icons
- One consistent line icon set (for example Phosphor) plus our own drawn map-tool icons. No emoji.

## Interaction
- Keyboard first: every menu item shows its shortcut; `[` and `]` change brush size; F3 opens command search.
- Brush cursor shows size and falloff rings.
- Empty states are one line of text and one button.
- UI text uses the words in `GLOSSARY.md`.
