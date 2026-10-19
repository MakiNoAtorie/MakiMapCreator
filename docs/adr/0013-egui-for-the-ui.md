# egui for the UI

The app UI uses egui, themed to the rules in `docs/design/ui.md`. The map canvas is drawn by our own wgpu renderer inside the egui layout; egui draws panels, controls and pages. The mockup in `spikes/ui-proto` confirmed that a dense, creative-tool look is reachable with egui styling alone.

We chose egui because it is immediate-mode (no widget state to keep in sync with the project data), it runs on wgpu, and its look is fully restylable. Qt would add a C++ toolchain, and Slint or iced would give less control over custom canvases and panels.

## Consequences

- Phase 0 decides between eframe and our own winit loop with `egui-winit` and `egui-wgpu`. The deciding test is WinTab: the window must receive WinTab packets (by subclassing the window procedure), and the map canvas must get pen events with full pressure.
- The page editor is a custom egui widget, not `TextEdit` (ADR-0012).
