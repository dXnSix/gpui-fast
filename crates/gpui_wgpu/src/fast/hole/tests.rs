//! Holes read back through the headless renderer (`VID-1`): transparent
//! black inside, corner radii respected, untouched outside, and what is
//! drawn later blends over the hole.

use gpui::{
    AtlasTextureId, AtlasTile, BorderStyle, Bounds, ContentMask, Corners, DevicePixels, Edges,
    Hsla, PlatformHeadlessRenderer as _, PolychromeSprite, Quad, ScaledPixels, Scene, Size, TileId,
    point, size,
};

use crate::WgpuHeadlessRenderer;

const SIZE: Size<DevicePixels> = Size {
    width: DevicePixels(64),
    height: DevicePixels(48),
};

fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn quad(bounds: Bounds<ScaledPixels>, color: Hsla) -> Quad {
    Quad {
        order: 0,
        border_style: BorderStyle::Solid,
        bounds,
        content_mask: ContentMask { bounds },
        background: color.into(),
        border_color: color,
        corner_radii: Corners::default(),
        border_widths: Edges::default(),
    }
}

fn hole(bounds: Bounds<ScaledPixels>, radius: f32) -> PolychromeSprite {
    PolychromeSprite {
        order: 0,
        pad: 0,
        tile: AtlasTile {
            texture_id: AtlasTextureId::hole(),
            tile_id: TileId(0),
            padding: 0,
            bounds: Bounds::default(),
        },
        grayscale: false.into(),
        opacity: 1.0,
        bounds,
        content_mask: ContentMask {
            bounds: rect(0., 0., 64., 48.),
        },
        corner_radii: Corners::all(ScaledPixels(radius)),
    }
}

/// A red window with a hole at (8, 8) 40×24, corner radius 8, and, when
/// `over`, a half-transparent blue quad drawn after it across the hole's
/// right edge.
fn scene(over: bool) -> Scene {
    let mut scene = Scene::default();
    scene.insert_primitive(quad(rect(0., 0., 64., 48.), gpui::red()));
    scene.insert_primitive(hole(rect(8., 8., 40., 24.), 8.));
    if over {
        scene.insert_primitive(quad(rect(40., 12., 20., 8.), gpui::blue().opacity(0.5)));
    }
    scene.finish();
    scene
}

fn render(scene: &Scene) -> image::RgbaImage {
    let mut renderer = WgpuHeadlessRenderer::new().expect("a headless renderer");
    renderer
        .render_scene_to_image(scene, SIZE)
        .expect("the scene renders")
}

const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn at(image: &image::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn a_hole_is_transparent_inside_and_leaves_the_outside() {
    let scene = scene(false);
    assert!(scene.has_holes());
    let image = render(&scene);
    assert_eq!(at(&image, 28, 20), CLEAR, "the middle");
    assert_eq!(at(&image, 9, 20), CLEAR, "inside the left edge");
    assert_eq!(at(&image, 4, 4), RED, "outside, above and left");
    assert_eq!(at(&image, 56, 40), RED, "outside, below and right");
    assert_eq!(at(&image, 28, 34), RED, "just below the hole");
}

#[test]
fn rounded_corners_keep_the_pixels_outside_the_radius() {
    let image = render(&scene(false));
    // The top-left corner pixel lies outside the 8 px radius.
    assert_eq!(at(&image, 8, 8), RED, "the corner itself");
    assert_eq!(at(&image, 47, 31), RED, "the bottom-right corner");
    // Inside the radius, next to the corner, the hole clears.
    assert_eq!(at(&image, 12, 12), CLEAR);
    // On the arc the old pixel fades out with the coverage.
    let edge = at(&image, 10, 10);
    assert!(edge[3] < 255 && edge[0] == edge[3], "antialiased: {edge:?}");
}

#[test]
fn what_is_drawn_after_a_hole_blends_over_it() {
    let image = render(&scene(true));
    let over_hole = at(&image, 44, 16);
    assert_eq!(over_hole[0], 0, "no red under it: {over_hole:?}");
    assert!(
        (126..=129).contains(&over_hole[2]) && (126..=129).contains(&over_hole[3]),
        "half blue over transparent black: {over_hole:?}"
    );
    let over_red = at(&image, 56, 16);
    assert!(
        over_red[0] > 100 && over_red[2] > 100 && over_red[3] == 255,
        "blue blended over red outside the hole: {over_red:?}"
    );
    assert_eq!(at(&image, 28, 20), CLEAR, "the rest of the hole");
}
