//! Terrain shading shared by the kinds' height passes: palette, hillshade, coastline (`terrain.wgsl`).

/// The coastline contour, metres; above 0 so a stroke that ends just above 0 m draws no coast.
pub const SHORE: f32 = 2.0;

/// (height in metres, RGB 0..1), sorted, linear between stops; muted atlas tints, both themes.
/// Sea is at or below 0 m; the shallows run to the land stop at `SHORE`.
pub const PALETTE: &[(f32, [f32; 3])] = &[
    (-6000.0, [0.247, 0.416, 0.604]), // #3f6a9a abyss
    (-3000.0, [0.353, 0.529, 0.710]), // #5a87b5
    (-1000.0, [0.494, 0.655, 0.800]), // #7ea7cc
    (-200.0, [0.647, 0.776, 0.878]),  // #a5c6e0 shelf
    (0.0, [0.765, 0.863, 0.925]),     // #c3dcec shallows
    (SHORE, [0.580, 0.749, 0.545]),   // #94bf8b lowland
    (150.0, [0.710, 0.812, 0.584]),   // #b5cf95
    (400.0, [0.863, 0.863, 0.651]),   // #dcdca6
    (800.0, [0.890, 0.788, 0.588]),   // #e3c996
    (1500.0, [0.812, 0.647, 0.486]),  // #cfa57c
    (2500.0, [0.690, 0.518, 0.396]),  // #b08465
    (3500.0, [0.627, 0.541, 0.486]),  // #a08a7c rock
    (4500.0, [0.910, 0.894, 0.878]),  // #e8e4e0
    (6000.0, [0.973, 0.973, 0.965]),  // #f8f8f6 snow
];

/// Coastline ink, #44586a.
pub const COAST: [f32; 3] = [0.267, 0.345, 0.416];

/// `terrain.wgsl` with `PALETTE`, `COAST` and `SHORE` as WGSL constants.
pub(crate) fn wgsl() -> String {
    let stops: Vec<String> = PALETTE
        .iter()
        .map(|(h, [r, g, b])| format!("vec4f({h:?}, {r:?}, {g:?}, {b:?})"))
        .collect();
    let [r, g, b] = COAST;
    format!(
        "const PALETTE_LEN = {0};\nconst PALETTE = array<vec4f, {0}>({1});\nconst COAST = vec3f({r:?}, {g:?}, {b:?});\nconst SHORE = {3:?};\n{2}",
        PALETTE.len(),
        stops.join(", "),
        include_str!("terrain.wgsl"),
        SHORE
    )
}
