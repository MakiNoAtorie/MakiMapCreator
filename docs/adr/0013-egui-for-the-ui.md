# egui for the UI

The app UI uses egui, themed to the rules in `docs/design/ui.md`. The map canvas is drawn by our own wgpu renderer inside the egui layout; egui draws panels, controls and pages. The mockup in `spikes/ui-proto` confirmed that a dense, creative-tool look is reachable with egui styling alone.

We chose egui because it is immediate-mode (no widget state to keep in sync with the project data), it runs on wgpu, and its look is fully restylable. Qt would add a C++ toolchain, and Slint or iced would give less control over custom canvases and panels.

## Consequences

- The app runs on eframe (wgpu backend), not on our own winit loop with `egui-winit` and `egui-wgpu`. The deciding test was WinTab: eframe gives the window handle at startup, so the app subclasses the window procedure (`SetWindowSubclass`), reads WinTab packets or Windows Ink `WM_POINTER` messages there, and queues them as one pen event type for the map canvas, with the full driver pressure range. eframe needs less code, and nothing in the pen path needs control of the loop. Change to our own loop only if a later need (for example, frame pacing for the map renderer) is blocked by eframe.
- The page editor is a custom egui widget, not `TextEdit` (ADR-0012).
