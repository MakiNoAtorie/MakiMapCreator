# Root space is in metres; height is f32 metres

Root space units are metres. Each map stores its metres per pixel. Height is stored as 32-bit floats in metres, sea level at 0. Scale bars, settlement sizes and distances are physical, and parent-to-child scaling is a ratio. We rejected normalized 0..1 height and 16-bit integers: they need a per-map scale to mean anything, and f32 tiles compress well enough with zstd.
