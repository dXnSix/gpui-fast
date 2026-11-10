// Modified by Longbridge for gpui-fast.
use crate::fast::hole::{HOLE_TEXTURE_INDEX, snapped, sprite};
use crate::{
    AtlasTextureId, AtlasTextureKind, Bounds, Corners, Pixels, ScaledPixels, Scene, point, size,
};

#[test]
fn the_hole_id_is_its_own() {
    let hole = AtlasTextureId::hole();
    assert!(hole.is_hole());
    assert!(hole.index < 0xE000_0000, "below the dynamic textures");
    let sprite_texture = AtlasTextureId {
        index: 3,
        kind: AtlasTextureKind::Polychrome,
    };
    assert!(!sprite_texture.is_hole());
    let monochrome = AtlasTextureId {
        index: HOLE_TEXTURE_INDEX,
        kind: AtlasTextureKind::Monochrome,
    };
    assert!(!monochrome.is_hole(), "only polychrome sprites are holes");
}

#[test]
fn bounds_snap_to_device_pixels() {
    let bounds = Bounds {
        origin: point(Pixels(10.3), Pixels(20.6)),
        size: size(Pixels(100.4), Pixels(50.2)),
    };
    let snapped = snapped(bounds, 2.0);
    assert_eq!(snapped.origin, point(ScaledPixels(21.), ScaledPixels(41.)));
    assert_eq!(
        snapped.size,
        size(ScaledPixels(200.), ScaledPixels(101.)),
        "each edge rounded (221.4 and 141.6), not the size (200.8 and 100.4)"
    );
}

#[test]
fn a_scene_reports_its_holes() {
    let mut scene = Scene::default();
    assert!(!scene.has_holes());
    let bounds = Bounds {
        origin: point(ScaledPixels(0.), ScaledPixels(0.)),
        size: size(ScaledPixels(10.), ScaledPixels(10.)),
    };
    scene.insert_primitive(sprite(
        bounds,
        crate::ContentMask { bounds },
        Corners::default(),
    ));
    scene.finish();
    assert!(scene.has_holes());
}
