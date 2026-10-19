# A map owns only the area inside its outline

A child map covers the bounding box of its outline on the parent plus a margin, but it owns only the area inside the outline. Outside the outline it shows read-only context, dimmed: the parent's data, or a neighbour's summary where a neighbour map exists. Every point in root space is owned by exactly one map at each scale, so overlapping rectangles never give two editable versions of the same ground.
