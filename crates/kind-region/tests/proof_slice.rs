//! Phase 0 sign-off: proof steps 1, 2, 4, 5, 6, 7 and 8 in order on one project, through the
//! `mmc-core` public API only (seam 1).

use std::time::Instant;

use mmc_core::{
    BrushStroke, Command, Entity, EntityEdit, Kinds, Layer, MapKind, MapKindModule, MapSettings,
    PageEdit, PaintStroke, Project, RootPoint, StrokePoint, link_markup,
};
use mmc_fixtures::{ManyEntities, Rng};
use mmc_kind_region::RegionKind;
use mmc_kind_world::WorldKind;

const KM: f64 = 1000.0;

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint {
        x: x * KM,
        y: y * KM,
    }
}

fn paint(layer: Layer, c: RootPoint, radius: f64, value: f32) -> Command {
    Command::Paint(PaintStroke {
        layer,
        points: vec![c],
        radius,
        value,
    })
}

fn brush(c: RootPoint) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::Height,
        points: vec![StrokePoint {
            at: c,
            pressure: 1.0,
        }],
        radius: 300.0,
        strength: 50.0,
    })
}

fn median_ms(mut f: impl FnMut()) -> f64 {
    let mut t: Vec<f64> = (0..7)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(f64::total_cmp);
    t[3]
}

#[test]
fn proof_slice_steps_1_2_4_5_6_7_8() {
    let dir = tempfile::tempdir().unwrap();
    let kinds = Kinds::new(vec![Box::new(WorldKind), Box::new(RegionKind)]);
    let settings = MapSettings {
        kind: MapKind::World,
        width: 64,
        height: 64,
        metres_per_pixel: KM,
    };
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings).unwrap();
    let w = p.maps()[0].id;

    // Step 1: paint land, generate crude regions as entities.
    p.apply(w, paint(Layer::Land, at(32.0, 32.0), 20.0 * KM, 1.0))
        .unwrap();
    let cmd = WorldKind.generate(&p, w, "regions", 7).unwrap();
    p.apply(w, cmd).unwrap();
    let outlines = p.outlines(w).unwrap();
    assert!(outlines.len() >= 2, "need two regions");
    let (region, outline) = outlines[0].clone();
    let other = outlines[1].0;

    // Step 2: child region map from parent context.
    let child = p.create_child_map(&kinds, w, region, None).unwrap();
    let cmd = kinds
        .get(MapKind::Region)
        .unwrap()
        .generate(&p, child, "from-parent", 3)
        .unwrap();
    p.apply(child, cmd).unwrap();
    assert!(p.edited_tiles(child, Layer::Height).unwrap().is_empty());

    // Step 4: the world shows the child's summary.
    assert!(
        p.child_summaries(&kinds, w)
            .unwrap()
            .iter()
            .any(|s| s.map == child)
    );

    // Step 5: a page links to another entity; renaming the target keeps the link.
    let name = p.entity(other).unwrap().unwrap().name;
    let text = format!("Neighbour: {}.", link_markup(other, &name));
    let edit = PageEdit {
        entity: region,
        text: text.clone(),
    };
    p.apply(child, Command::Page(edit)).unwrap();
    let rename = EntityEdit::Rename {
        id: other,
        name: "Renamed Vale".into(),
    };
    p.apply(child, Command::Entity(rename)).unwrap();
    let links = p.links(region).unwrap();
    assert_eq!(links[0].target, other);
    assert_eq!(links[0].entity.as_ref().unwrap().name, "Renamed Vale");
    assert_eq!(p.backlinks(other).unwrap(), [region]);

    // Step 6: save while a stroke is applied, close, reload.
    let c = outline
        .iter()
        .fold((0.0, 0.0), |a, q| (a.0 + q.x, a.1 + q.y));
    let n = outline.len() as f64;
    let centre = RootPoint {
        x: c.0 / n,
        y: c.1 / n,
    };
    let file = dir.path().join("world.mmc");
    p.save(&file);
    p.apply(child, brush(centre)).unwrap();
    p.wait_for_save().unwrap();
    let maps = p.maps().to_vec();
    p.close().unwrap();
    let mut p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(p.maps(), maps.as_slice());
    assert_eq!(p.page(region).unwrap(), text);
    assert_eq!(p.backlinks(other).unwrap(), [region]);
    assert_eq!(p.outlines(w).unwrap().len(), outlines.len());
    // The stroke began after the save did: the file holds it whole or not at all.
    // Re-apply it when the snapshot missed it, so step 8 has a hand edit.
    if p.edited_tiles(child, Layer::Height).unwrap().is_empty() {
        p.apply(child, brush(centre)).unwrap();
    }
    let edited = p.edited_tiles(child, Layer::Height).unwrap();
    assert!(!edited.is_empty());

    // Step 7: search a place name among 10 000 entities within the ADR-0014 budgets.
    let mut rng = Rng::new(11);
    for i in 0..10_000 {
        let id = p.next_entity_id().unwrap();
        let e = Entity {
            id,
            kind: "settlement".into(),
            name: ManyEntities::name(&mut rng),
            fields: Default::default(),
        };
        p.apply(w, Command::Entity(EntityEdit::Create(e))).unwrap();
        if i == 0 {
            let edit = PageEdit {
                entity: id,
                text: "A gate called Zephyrgate.".into(),
            };
            p.apply(w, Command::Page(edit)).unwrap();
        }
    }
    // Debug builds are slower; the gate in mmc-bench holds the real numbers.
    let slack = if cfg!(debug_assertions) { 10.0 } else { 1.0 };
    assert!(!p.find_names("renamed", 5).is_empty());
    let t = median_ms(|| {
        p.find_names("renamed", 10);
    });
    assert!(t < 16.0 * slack, "name lookup {t} ms");
    let hits = |p: &Project| p.search_text("zephyrgate", 10).unwrap().wait().unwrap();
    assert_eq!(hits(&p).len(), 1);
    let t = median_ms(|| {
        hits(&p);
    });
    assert!(t < 50.0 * slack, "full text {t} ms");

    // Step 8: edit the world inside the region; the child is out of date; re-sync keeps edits.
    assert!(!p.is_out_of_date(child).unwrap());
    let land = p.layer_value(w, Layer::Land, centre).unwrap().unwrap();
    p.apply(w, paint(Layer::Land, centre, 1.5 * KM, 1.0 - land.round()))
        .unwrap();
    assert!(p.is_out_of_date(child).unwrap());
    let tiles = |p: &Project| {
        edited
            .iter()
            .map(|&(tx, ty)| p.layer_tile(child, Layer::Height, tx, ty).unwrap())
            .collect::<Vec<_>>()
    };
    let before = tiles(&p);
    let plan = p.preview_resync(&kinds, child, None).unwrap();
    assert!(plan.lost_edited.is_empty());
    for &(tx, ty) in &edited {
        assert!(plan.kept_edited.contains(&(Layer::Height, tx, ty)));
    }
    p.apply_plan(plan, false).unwrap();
    assert!(!p.is_out_of_date(child).unwrap());
    assert_eq!(before, tiles(&p));

    // Masked regeneration leaves locked pixels alone.
    p.apply(child, paint(Layer::Lock, centre, 300.0, 1.0))
        .unwrap();
    let locked = p.layer_value(child, Layer::Height, centre).unwrap();
    let plan = p
        .preview_regeneration(&kinds, child, "from-parent", 99, None)
        .unwrap();
    p.apply_plan(plan, false).unwrap();
    assert_eq!(p.layer_value(child, Layer::Height, centre).unwrap(), locked);
}
