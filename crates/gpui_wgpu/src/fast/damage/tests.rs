use gpui::{
    AtlasTextureId, AtlasTextureKind, AtlasTile, BorderStyle, Bounds, ContentMask, Corners,
    DevicePixels, Edges, Hsla, MonochromeSprite, Quad, ScaledPixels, Scene, Size, TileId,
    TransformationMatrix, point, size,
};

use super::{Damage, DamageTracker, MAX_EDITS, MAX_RECTS, diff, merge};

const WINDOW: Size<DevicePixels> = Size {
    width: DevicePixels(1920),
    height: DevicePixels(1080),
};

fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn device(x: i32, y: i32, width: i32, height: i32) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(DevicePixels(x), DevicePixels(y)),
        size: size(DevicePixels(width), DevicePixels(height)),
    }
}

fn color(hue: f32) -> Hsla {
    gpui::hsla(hue, 0.5, 0.5, 1.)
}

fn quad(x: f32, y: f32, width: f32, height: f32, hue: f32) -> Quad {
    let bounds = bounds(x, y, width, height);
    Quad {
        order: 0,
        border_style: BorderStyle::Solid,
        bounds,
        content_mask: ContentMask {
            bounds: self::bounds(0., 0., 1920., 1080.),
        },
        background: color(hue).into(),
        border_color: color(hue),
        corner_radii: Corners::default(),
        border_widths: Edges::default(),
    }
}

fn glyph(x: f32, y: f32) -> MonochromeSprite {
    MonochromeSprite {
        order: 0,
        pad: 0,
        tile: AtlasTile {
            texture_id: AtlasTextureId {
                index: 0,
                kind: AtlasTextureKind::Monochrome,
            },
            tile_id: TileId(7),
            padding: 0,
            bounds: device(0, 0, 10, 12),
        },
        bounds: bounds(x, y, 10., 12.),
        content_mask: ContentMask {
            bounds: bounds(0., 0., 1920., 1080.),
        },
        color: color(0.1),
        transformation: TransformationMatrix::unit(),
    }
}

fn scene(quads: &[Quad], glyphs: &[MonochromeSprite]) -> Scene {
    let mut scene = Scene::default();
    for quad in quads {
        scene.insert_primitive(*quad);
    }
    for glyph in glyphs {
        scene.insert_primitive(*glyph);
    }
    scene.finish();
    scene
}

/// A tracker that has seen `first`.
fn tracker(first: &Scene) -> DamageTracker {
    let mut tracker = DamageTracker::new(true);
    assert_eq!(
        tracker.frame(first, WINDOW),
        Damage::Full,
        "the first frame"
    );
    tracker
}

fn covers(rects: &[Bounds<DevicePixels>], area: Bounds<DevicePixels>) -> bool {
    rects.iter().any(|r| {
        r.origin.x <= area.origin.x
            && r.origin.y <= area.origin.y
            && r.origin.x.0 + r.size.width.0 >= area.origin.x.0 + area.size.width.0
            && r.origin.y.0 + r.size.height.0 >= area.origin.y.0 + area.size.height.0
    })
}

fn rects(damage: Damage) -> Vec<Bounds<DevicePixels>> {
    match damage {
        Damage::Rects(rects) => rects,
        Damage::Full => panic!("expected partial damage, got the whole window"),
    }
}

#[test]
fn unchanged_scenes_have_no_damage() {
    let quads = [
        quad(10., 10., 100., 40., 0.1),
        quad(10., 60., 100., 40., 0.2),
    ];
    let mut tracker = tracker(&scene(&quads, &[glyph(20., 20.)]));
    assert_eq!(
        tracker.frame(&scene(&quads, &[glyph(20., 20.)]), WINDOW),
        Damage::Rects(Vec::new())
    );
}

#[test]
fn a_changed_quad_damages_its_bounds_only() {
    let row = |hue| {
        [
            quad(0., 0., 1920., 1080., 0.9),
            quad(100., 200., 300., 50., hue),
        ]
    };
    let mut tracker = tracker(&scene(&row(0.1), &[]));
    let damage = rects(tracker.frame(&scene(&row(0.6), &[]), WINDOW));
    assert_eq!(
        damage,
        [device(99, 199, 302, 52)],
        "the quad and its antialiasing"
    );
}

#[test]
fn a_moved_sprite_damages_where_it_was_and_where_it_is() {
    let background = [quad(0., 0., 1920., 1080., 0.9)];
    let mut tracker = tracker(&scene(&background, &[glyph(100., 100.)]));
    let damage = rects(tracker.frame(&scene(&background, &[glyph(500., 100.)]), WINDOW));
    assert_eq!(damage.len(), 2);
    assert!(covers(&damage, device(100, 100, 10, 12)), "{damage:?}");
    assert!(covers(&damage, device(500, 100, 10, 12)), "{damage:?}");
    assert!(
        !covers(&damage, device(300, 100, 10, 12)),
        "nothing in between"
    );
}

#[test]
fn a_clip_change_damages_the_clipped_primitive() {
    let mut clipped = quad(100., 100., 400., 100., 0.3);
    let mut tracker = tracker(&scene(&[clipped], &[]));
    clipped.content_mask.bounds = bounds(100., 100., 200., 100.);
    let damage = rects(tracker.frame(&scene(&[clipped], &[]), WINDOW));
    // The new clip shows less: what it hides must be redrawn too.
    assert!(covers(&damage, device(100, 100, 400, 100)), "{damage:?}");
}

#[test]
fn an_inserted_primitive_does_not_damage_those_after_it() {
    let rows: Vec<Quad> = (0..20)
        .map(|i| quad(0., 40. * i as f32, 600., 30., 0.05 * i as f32))
        .collect();
    let mut tracker = tracker(&scene(&rows, &[]));
    let mut more = vec![quad(1000., 500., 50., 50., 0.7)];
    more.extend_from_slice(&rows);
    let damage = rects(tracker.frame(&scene(&more, &[]), WINDOW));
    assert_eq!(damage, [device(999, 499, 52, 52)], "only the new quad");
}

#[test]
fn many_small_changes_merge_into_a_bounded_list() {
    let dots = |hue: f32| -> Vec<Quad> {
        (0..40)
            .map(|i| quad(45. * i as f32, 30. * (i % 7) as f32, 8., 8., hue))
            .collect()
    };
    let mut tracker = tracker(&scene(&dots(0.1), &[]));
    let damage = rects(tracker.frame(&scene(&dots(0.5), &[]), WINDOW));
    assert!(damage.len() <= MAX_RECTS, "{} rectangles", damage.len());
    for i in 0..40 {
        let dot = device(45 * i, 30 * (i % 7), 8, 8);
        assert!(covers(&damage, dot), "dot {i} at {dot:?} in {damage:?}");
    }
}

#[test]
fn the_whole_window_when_forced() {
    let quads = [quad(10., 10., 100., 40., 0.1)];
    // GPUI_PARTIAL_REDRAW=0.
    let mut off = DamageTracker::new(false);
    off.frame(&scene(&quads, &[]), WINDOW);
    assert_eq!(off.frame(&scene(&quads, &[]), WINDOW), Damage::Full);
    // A resize.
    let mut resized = tracker(&scene(&quads, &[]));
    let smaller = size(DevicePixels(1280), DevicePixels(720));
    assert_eq!(resized.frame(&scene(&quads, &[]), smaller), Damage::Full);
    assert_eq!(
        resized.frame(&scene(&quads, &[]), smaller),
        Damage::Rects(Vec::new())
    );
    // Atlas loss, a scale change or a lost target.
    resized.invalidate();
    assert_eq!(resized.frame(&scene(&quads, &[]), smaller), Damage::Full);
    // Too many changes to diff.
    let many = |hue: f32| -> Vec<Quad> {
        (0..MAX_EDITS)
            .map(|i| quad((i % 100) as f32, (i / 100) as f32, 1., 1., hue))
            .collect()
    };
    let mut busy = tracker(&scene(&many(0.1), &[]));
    assert_eq!(busy.frame(&scene(&many(0.6), &[]), WINDOW), Damage::Full);
    assert_eq!(
        Damage::Full.rects(WINDOW),
        [device(0, 0, 1920, 1080)],
        "the whole window as a rectangle"
    );
}

#[test]
fn a_transformed_sprite_damages_the_whole_window() {
    let mut turned = glyph(100., 100.);
    turned.transformation = TransformationMatrix::unit().rotate(gpui::Radians(0.5));
    let mut tracker = tracker(&scene(&[], &[turned]));
    turned.color = color(0.8);
    assert_eq!(tracker.frame(&scene(&[], &[turned]), WINDOW), Damage::Full);
}

#[test]
fn the_diff_keeps_the_longest_common_subsequence() {
    let (a, b) = diff(&[1, 2, 3, 4, 5], &[1, 3, 4, 9, 5], 16).unwrap();
    assert_eq!(a, [true, false, true, true, true]);
    assert_eq!(b, [true, true, true, false, true]);
    let (a, b) = diff(&[], &[7, 8], 16).unwrap();
    assert!(a.is_empty());
    assert_eq!(b, [false, false]);
    let (a, b) = diff(&[1, 2], &[1, 2], 0).unwrap();
    assert_eq!((a, b), (vec![true, true], vec![true, true]));
    assert!(diff(&[1, 2, 3], &[4, 5, 6], 5).is_none(), "six edits");
}

#[test]
fn merged_rectangles_stay_inside_the_window() {
    let damage = merge(
        vec![
            bounds(-50., -50., 100., 100.),
            bounds(1900., 1000., 100., 100.),
        ],
        WINDOW,
    );
    assert_eq!(
        damage,
        Damage::Rects(vec![device(0, 0, 50, 50), device(1900, 1000, 20, 80)])
    );
    assert_eq!(
        merge(vec![bounds(-1., -1., 3000., 2000.)], WINDOW),
        Damage::Full
    );
    assert_eq!(
        merge(vec![bounds(5000., 5000., 10., 10.)], WINDOW),
        Damage::Rects(vec![])
    );
}
