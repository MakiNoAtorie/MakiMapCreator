//! Proof steps 4 and 8 (seam 1): summaries, out of date, re-sync, lock mask, masked
//! regeneration and moved outlines (ADR-0002, ADR-0003, ADR-0022).

use mmc_core::{
    BrushStroke, Command, Error, GenRun, Generation, Kinds, Layer, MapId, MapKindModule,
    MapSettings, NewEntity, PaintStroke, Project, RootPoint, StrokePoint,
};
use mmc_kind_region::{REGION, RegionKind, SUMMARY_ALPHA};
use mmc_kind_world::{LAND, WORLD, WorldKind};

const KM: f64 = 1000.0;
/// Child scale: 480 px for a 12 km child, so 2 × 2 tiles.
const MPP: f64 = 25.0;

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint {
        x: x * KM,
        y: y * KM,
    }
}

fn kinds() -> Kinds {
    Kinds::new(vec![Box::new(WorldKind), Box::new(RegionKind)])
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<RootPoint> {
    vec![at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]
}

fn paint(layer: Layer, c: RootPoint, radius_km: f64, value: f32) -> Command {
    Command::Paint(PaintStroke {
        layer,
        points: vec![c],
        radius: radius_km * KM,
        value,
    })
}

fn brush(c: RootPoint) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: c,
            pressure: 1.0,
            radius: 300.0,
        }],
        strength: 50.0,
        falloff: 1.0,
    })
}

fn height(p: &Project, map: MapId, x: f64, y: f64) -> f32 {
    p.layer_value(map, Layer::HEIGHT, at(x, y))
        .unwrap()
        .unwrap()
}

/// A 32 km world with land around (12, 0) km, regions West (4..12 km) and East (12..20 km),
/// and a generated child map of each, West first.
fn setup(dir: &std::path::Path) -> (Project, MapId, MapId, MapId) {
    let kinds = kinds();
    let world = MapSettings {
        kind: WORLD,
        width: 32,
        height: 32,
        metres_per_pixel: KM,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), world).unwrap();
    let w = p.maps()[0].id;
    p.apply(w, paint(LAND, at(12.0, 0.0), 11.0, 1.0)).unwrap();
    let region = |name: &str, outline| NewEntity {
        kind: "region".into(),
        name: name.into(),
        outline,
    };
    p.apply(
        w,
        Command::Generate(Generation {
            run: GenRun::new("test", 1),
            entities: vec![
                region("West", rect(4.0, 4.0, 12.0, 12.0)),
                region("East", rect(12.0, 4.0, 20.0, 12.0)),
            ],
            rasters: vec![],
        }),
    )
    .unwrap();
    let ids: Vec<_> = p.outlines(w).unwrap().iter().map(|o| o.0).collect();
    let mut children = vec![];
    for e in ids {
        let c = p.create_child_map(w, e, Some(MPP)).unwrap();
        let cmd = kinds
            .get(REGION)
            .unwrap()
            .generate(&p, c, &GenRun::new("from-parent", 5))
            .unwrap();
        p.apply(c, cmd).unwrap();
        children.push(c);
    }
    (p, w, children[0], children[1])
}

#[test]
fn proof_step_4_the_world_map_shows_the_child_summary() {
    let dir = tempfile::tempdir().unwrap();
    let (p, w, west, east) = setup(dir.path());
    let s = p.child_summaries(w).unwrap();
    assert_eq!(
        s.iter().map(|s| s.map).collect::<Vec<_>>(),
        vec![west, east]
    );
    assert!(s[0].contains(at(8.0, 8.0)) && !s[0].contains(at(13.0, 8.0)));
    // West is mostly land, so its tint leans green; it is see-through.
    let [r, g, b, a] = s[0].summary.tint;
    assert!(g > b && g > r, "{:?}", s[0].summary.tint);
    assert_eq!(a, SUMMARY_ALPHA);
    // A child map has no children to summarise.
    assert!(p.child_summaries(west).unwrap().is_empty());
}

#[test]
fn a_parent_edit_inside_a_childs_area_marks_only_that_child_out_of_date() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, w, west, east) = setup(dir.path());
    assert!(!p.is_out_of_date(west).unwrap() && !p.is_out_of_date(east).unwrap());
    assert!(!p.is_out_of_date(w).unwrap());

    // Outside both children's areas: nothing is flagged.
    p.apply(w, paint(LAND, at(28.0, 28.0), 1.5, 1.0)).unwrap();
    assert!(!p.is_out_of_date(west).unwrap() && !p.is_out_of_date(east).unwrap());

    // Inside West's margin (outside its outline), but not East's area.
    p.apply(w, paint(LAND, at(2.5, 8.0), 0.6, 1.0)).unwrap();
    assert!(p.is_out_of_date(west).unwrap());
    assert!(!p.is_out_of_date(east).unwrap());

    // Child edits never flag their parent or siblings.
    p.apply(east, brush(at(16.0, 8.0))).unwrap();
    assert!(!p.is_out_of_date(east).unwrap());

    // The flag is stored.
    let file = dir.path().join("w.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert!(p.is_out_of_date(west).unwrap());
    assert!(!p.is_out_of_date(east).unwrap());
}

#[test]
fn the_lock_mask_is_an_authored_layer_painted_with_the_paint_command() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _, west, _) = setup(dir.path());
    assert!(!p.is_derived(west, Layer::LOCK).unwrap());
    assert!(RegionKind.layers().contains(&Layer::LOCK));
    assert!(RegionKind.tools().iter().any(|t| t.layer == Layer::LOCK));
    p.apply(west, paint(Layer::LOCK, at(6.0, 6.0), 0.3, 1.0))
        .unwrap();
    assert_eq!(
        p.layer_value(west, Layer::LOCK, at(6.0, 6.0)).unwrap(),
        Some(1.0)
    );
    assert_eq!(
        p.layer_value(west, Layer::LOCK, at(7.0, 7.0)).unwrap(),
        Some(0.0)
    );
    assert!(p.undo(west).unwrap());
    assert_eq!(
        p.layer_value(west, Layer::LOCK, at(6.0, 6.0)).unwrap(),
        Some(0.0)
    );
}

#[test]
fn proof_step_8_resync_keeps_edited_tiles_and_locked_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let _kinds = kinds();
    let (mut p, w, west, _) = setup(dir.path());
    // A hand edit in tile (1, 1) and a lock in tile (0, 0).
    p.apply(west, brush(at(10.0, 10.0))).unwrap();
    p.apply(west, paint(Layer::LOCK, at(5.5, 5.5), 0.3, 1.0))
        .unwrap();
    assert_eq!(p.edited_tiles(west, Layer::HEIGHT).unwrap(), vec![(1, 1)]);
    let edited = p.layer_tile(west, Layer::HEIGHT, 1, 1).unwrap();
    let (locked, free) = (height(&p, west, 5.5, 5.5), height(&p, west, 6.5, 6.5));
    assert!(locked > 0.0 && free > 0.0);

    // The world turns the land around (6, 6) km into sea, under both points, in tile (0, 0).
    p.apply(w, paint(LAND, at(6.0, 6.0), 1.2, 0.0)).unwrap();
    assert!(p.is_out_of_date(west).unwrap());

    // The preview lists what changes and what is kept, and writes nothing.
    let plan = p.preview_resync(west, None).unwrap();
    assert_eq!(plan.map, west);
    assert_eq!(plan.regenerate, vec![(Layer::HEIGHT, 0, 0)]);
    assert_eq!(plan.kept_edited, vec![(Layer::HEIGHT, 1, 1)]);
    assert_eq!(plan.kept_locked, vec![(Layer::HEIGHT, 0, 0)]);
    assert!(plan.lost_edited.is_empty());
    assert!(p.is_out_of_date(west).unwrap());
    assert_eq!(height(&p, west, 6.5, 6.5), free);

    p.apply_plan(plan, false).unwrap();
    assert!(!p.is_out_of_date(west).unwrap());
    assert!(
        height(&p, west, 6.5, 6.5) < 0.0,
        "unedited ground follows the parent"
    );
    assert_eq!(height(&p, west, 5.5, 5.5), locked);
    assert_eq!(p.layer_tile(west, Layer::HEIGHT, 1, 1).unwrap(), edited);
    assert_eq!(p.edited_tiles(west, Layer::HEIGHT).unwrap(), vec![(1, 1)]);
    // The new snapshot is the parent's sea.
    let ctx = p.parent_context(west, LAND).unwrap().unwrap();
    assert_eq!(ctx.sample(at(6.5, 6.5)), 0.0);
    assert_eq!(p.generations(west).unwrap().last().unwrap().run.seed, 5);

    // One command: undo brings back the old ground, snapshot and flag.
    assert!(p.undo(west).unwrap());
    assert_eq!(height(&p, west, 6.5, 6.5), free);
    assert!(p.is_out_of_date(west).unwrap());
    assert_eq!(
        p.parent_context(west, LAND)
            .unwrap()
            .unwrap()
            .sample(at(6.5, 6.5)),
        1.0
    );
    assert!(p.redo(west).unwrap());
    assert!(height(&p, west, 6.5, 6.5) < 0.0);
    assert!(!p.is_out_of_date(west).unwrap());
}

#[test]
fn masked_regeneration_runs_only_inside_the_selection_or_the_unlocked_area() {
    let dir = tempfile::tempdir().unwrap();
    let _kinds = kinds();
    let (mut p, _, west, _) = setup(dir.path());
    let (inside, outside) = (height(&p, west, 6.0, 6.0), height(&p, west, 9.0, 6.0));

    let sel = rect(5.0, 5.0, 7.0, 7.0);
    let plan = p
        .preview_regeneration(west, &GenRun::new("from-parent", 99), Some(&sel))
        .unwrap();
    assert_eq!(
        height(&p, west, 6.0, 6.0),
        inside,
        "the preview writes nothing"
    );
    p.apply_plan(plan, false).unwrap();
    assert_ne!(height(&p, west, 6.0, 6.0), inside);
    assert_eq!(height(&p, west, 9.0, 6.0), outside);
    assert_eq!(p.generations(west).unwrap().last().unwrap().run.seed, 99);

    // No selection: everything but locked pixels and edited tiles.
    p.apply(west, paint(Layer::LOCK, at(9.0, 6.0), 0.3, 1.0))
        .unwrap();
    p.apply(west, brush(at(10.0, 10.0))).unwrap();
    let edited = p.layer_tile(west, Layer::HEIGHT, 1, 1).unwrap();
    let inside = height(&p, west, 6.0, 6.0);
    let plan = p
        .preview_regeneration(west, &GenRun::new("from-parent", 7), None)
        .unwrap();
    p.apply_plan(plan, false).unwrap();
    assert_eq!(height(&p, west, 9.0, 6.0), outside);
    assert_ne!(height(&p, west, 6.0, 6.0), inside);
    assert_eq!(p.layer_tile(west, Layer::HEIGHT, 1, 1).unwrap(), edited);
}

#[test]
fn a_moved_outline_generates_gained_ground_and_clears_lost_ground_only_on_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let _kinds = kinds();
    let (mut p, w, west, _) = setup(dir.path());
    let generated = height(&p, west, 11.5, 8.0);
    // Edits on ground the move loses (tile (1, 0)) and next to ground it gains (tile (0, 1)).
    p.apply(west, brush(at(11.5, 8.0))).unwrap();
    p.apply(west, brush(at(5.0, 8.5))).unwrap();
    let edited = height(&p, west, 11.5, 8.0);
    assert_ne!(edited, generated);
    // The parent turns gained ground at (3.5, 8.5) km into land.
    let gained = height(&p, west, 3.5, 8.5);
    assert!(gained < 0.0);
    p.apply(w, paint(LAND, at(3.5, 8.5), 0.4, 1.0)).unwrap();

    let moved = rect(3.0, 4.0, 11.0, 12.0);
    let plan = p.preview_resync(west, Some(moved.clone())).unwrap();
    assert_eq!(plan.lost_edited, vec![(Layer::HEIGHT, 1, 0)]);
    assert!(plan.lost_ground.contains(&(Layer::HEIGHT, 1, 1)));
    assert!(plan.resize.is_none());
    // Unedited lost ground stays too until confirmed.
    let unedited_lost = height(&p, west, 11.5, 11.0);
    assert!(plan.regenerate.contains(&(Layer::HEIGHT, 0, 1)));
    p.apply_plan(plan, false).unwrap();
    let m = p.maps().iter().find(|m| m.id == west).unwrap();
    assert_eq!(m.outline.as_deref(), Some(moved.as_slice()));
    assert!(p.owns(west, at(3.5, 8.5)).unwrap());
    assert!(!p.owns(west, at(11.5, 8.0)).unwrap());
    assert!(
        height(&p, west, 3.5, 8.5) > 0.0,
        "gained ground is generated"
    );
    assert_eq!(height(&p, west, 11.5, 8.0), edited, "kept without confirm");
    assert_eq!(height(&p, west, 11.5, 11.0), unedited_lost);

    // With confirm, data on lost ground is cleared: the parent context shows there.
    assert!(p.undo(west).unwrap());
    let plan = p.preview_resync(west, Some(moved)).unwrap();
    p.apply_plan(plan, true).unwrap();
    assert_eq!(height(&p, west, 11.5, 8.0), 0.0);
    assert_eq!(height(&p, west, 11.5, 11.0), 0.0);
    assert_ne!(generated, 0.0);
    assert!(
        p.edited_tiles(west, Layer::HEIGHT)
            .unwrap()
            .contains(&(0, 1))
    );
}

#[test]
fn an_outline_past_the_child_map_grows_the_map_and_undo_shrinks_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _, west, _) = setup(dir.path());
    p.apply(west, brush(at(6.0, 6.0))).unwrap();
    let edited = height(&p, west, 6.0, 6.0);
    let generated = height(&p, west, 10.0, 10.0);
    let before = p.maps().iter().find(|m| m.id == west).unwrap().clone();

    // Two km further west than the map reaches.
    let plan = p
        .preview_resync(west, Some(rect(1.0, 4.0, 11.0, 12.0)))
        .unwrap();
    let (origin, w, _) = plan.resize.unwrap();
    assert!(origin.x < before.origin.x && w > before.settings.width);
    p.apply_plan(plan, false).unwrap();
    let m = p.maps().iter().find(|m| m.id == west).unwrap().clone();
    assert_eq!(m.origin, origin);
    assert!(p.owns(west, at(1.5, 8.0)).unwrap());
    assert!(
        p.layer_value(west, Layer::HEIGHT, at(1.5, 8.0))
            .unwrap()
            .is_some()
    );
    // Stored data keeps its root-space place.
    assert_eq!(height(&p, west, 6.0, 6.0), edited);
    assert_eq!(height(&p, west, 10.0, 10.0), generated);
    p.wait_for_jobs().unwrap();

    assert!(p.undo(west).unwrap());
    let back = p.maps().iter().find(|m| m.id == west).unwrap().clone();
    assert_eq!(back, before);
    assert_eq!(height(&p, west, 6.0, 6.0), edited);
    assert!(p.redo(west).unwrap());
    assert_eq!(height(&p, west, 6.0, 6.0), edited);
    p.wait_for_jobs().unwrap();
    p.wait_tiles();

    // The grown map survives save and reopen.
    let file = dir.path().join("grown.mmc");
    p.save(&file);
    p.close().unwrap();
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let r = q.maps().iter().find(|m| m.id == west).unwrap();
    assert_eq!(r.origin, origin);
    assert_eq!(height(&q, west, 6.0, 6.0), edited);
}

#[test]
fn a_plan_the_map_changed_under_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _kinds = kinds();
    let (mut p, w, west, _) = setup(dir.path());
    p.apply(w, paint(LAND, at(6.0, 6.0), 2.0, 0.0)).unwrap();
    let plan = p.preview_resync(west, None).unwrap();
    p.apply(west, brush(at(6.0, 6.0))).unwrap();
    let err = p.apply_plan(plan, false).unwrap_err();
    assert!(matches!(err, Error::StalePlan), "{err}");
    assert!(p.is_out_of_date(west).unwrap());
    // A root map has nothing to re-sync with.
    assert!(p.preview_resync(w, None).is_err());
}
