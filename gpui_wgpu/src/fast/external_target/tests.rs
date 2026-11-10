// Modified by Longbridge for gpui-fast.
use std::collections::VecDeque;

use gpui::{
    BorderStyle, Bounds, ContentMask, Corners, DevicePixels, Edges, Hsla, Quad, ScaledPixels,
    Scene, Size, point, size,
};

use gpui::PlatformHeadlessRenderer as _;

use super::{Region, WgpuExternalRenderer, WgpuFrameDamage, flip_rect, redraw_region};
use crate::WgpuHeadlessRenderer;
use crate::wgpu_context::WgpuContext;

const SIZE: Size<DevicePixels> = Size {
    width: DevicePixels(64),
    height: DevicePixels(48),
};
const MAGENTA: [u8; 4] = [255, 0, 255, 255];

fn device_rect(x: i32, y: i32, width: i32, height: i32) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(DevicePixels(x), DevicePixels(y)),
        size: size(DevicePixels(width), DevicePixels(height)),
    }
}

fn history(
    frames: &[Option<&[Bounds<DevicePixels>]>],
) -> VecDeque<Option<Vec<Bounds<DevicePixels>>>> {
    frames.iter().map(|f| f.map(<[_]>::to_vec)).collect()
}

#[test]
fn the_redraw_region_follows_the_target_age() {
    let a = device_rect(0, 0, 10, 10);
    let b = device_rect(20, 20, 5, 5);
    let frames = history(&[Some(&[a]), Some(&[b]), Some(&[])]);
    assert_eq!(redraw_region(&frames, 0, SIZE), Region::Full, "unknown age");
    assert_eq!(redraw_region(&frames, 1, SIZE), Region::Partial(a));
    assert_eq!(
        redraw_region(&frames, 2, SIZE),
        Region::Partial(device_rect(0, 0, 25, 25)),
        "the bounding rectangle of two frames' damage"
    );
    assert_eq!(
        redraw_region(&frames, 4, SIZE),
        Region::Full,
        "older than the history"
    );
    let quiet = history(&[Some(&[]), Some(&[])]);
    assert_eq!(redraw_region(&quiet, 2, SIZE), Region::Nothing);
    let whole = history(&[Some(&[a]), None]);
    assert_eq!(redraw_region(&whole, 1, SIZE), Region::Partial(a));
    assert_eq!(
        redraw_region(&whole, 2, SIZE),
        Region::Full,
        "a frame damaged whole"
    );
    let all = history(&[Some(&[device_rect(0, 0, 64, 48)])]);
    assert_eq!(redraw_region(&all, 1, SIZE), Region::Full);
    assert_eq!(
        flip_rect(device_rect(4, 6, 10, 8), 48),
        device_rect(4, 34, 10, 8)
    );
}

fn quad(x: f32, y: f32, width: f32, height: f32, color: Hsla) -> Quad {
    let bounds = Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    };
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

/// An opaque background with a block at (16, 16) and one at (40, 24).
fn scene(block: Hsla, other: Hsla) -> Scene {
    let mut scene = Scene::default();
    scene.insert_primitive(quad(0., 0., 64., 48., gpui::black()));
    scene.insert_primitive(quad(16., 8., 16., 12., block));
    scene.insert_primitive(quad(40., 24., 12., 16., other));
    scene.finish();
    scene
}

/// A renderer on a device of its own, and a target texture it can draw into.
struct Harness {
    renderer: WgpuExternalRenderer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        let (context, format) = WgpuContext::create_headless()?;
        let device = (*context.device).clone();
        let queue = (*context.queue).clone();
        let WgpuContext {
            instance, adapter, ..
        } = context;
        let renderer =
            WgpuExternalRenderer::new(instance, adapter, device.clone(), queue.clone(), format)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("external_target"),
            size: wgpu::Extent3d {
                width: SIZE.width.0 as u32,
                height: SIZE.height.0 as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            renderer,
            texture,
            view,
            device,
            queue,
            format,
        })
    }

    fn draw(&mut self, scene: &Scene, flip_y: bool, age: u32) -> Vec<Bounds<DevicePixels>> {
        self.frame(scene, flip_y, age).damage
    }

    fn frame(&mut self, scene: &Scene, flip_y: bool, age: u32) -> WgpuFrameDamage {
        let mut told = None;
        let frame = self
            .renderer
            .draw(scene, &self.view, SIZE, flip_y, age, |damage| {
                told = Some(damage.clone())
            })
            .expect("the frame draws");
        assert_eq!(
            told.as_ref(),
            Some(&frame),
            "the same damage, before drawing"
        );
        frame
    }

    /// Paints the whole target magenta, standing in for contents a partial
    /// frame must keep.
    fn mark(&self) {
        let pixel = self.to_format(MAGENTA);
        let data: Vec<u8> = std::iter::repeat_n(pixel, (SIZE.width.0 * SIZE.height.0) as usize)
            .flatten()
            .collect();
        self.queue.write_texture(
            self.texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE.width.0 as u32 * 4),
                rows_per_image: None,
            },
            self.texture.size(),
        );
        self.queue.submit(std::iter::empty());
    }

    fn to_format(&self, rgba: [u8; 4]) -> [u8; 4] {
        match self.format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
                [rgba[2], rgba[1], rgba[0], rgba[3]]
            }
            _ => rgba,
        }
    }

    /// The target's pixels, RGBA, row by row.
    fn read(&self) -> image::RgbaImage {
        let (width, height) = (SIZE.width.0 as u32, SIZE.height.0 as u32);
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            self.texture.size(),
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the readback finishes");
        let data = buffer.slice(..).get_mapped_range().unwrap();
        let mut image = image::RgbaImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let at = (y * padded + x * 4) as usize;
                let pixel = [data[at], data[at + 1], data[at + 2], data[at + 3]];
                image.put_pixel(x, y, image::Rgba(self.to_format(pixel)));
            }
        }
        image
    }
}

/// What the surface path draws for `scene`.
fn reference(scene: &Scene) -> image::RgbaImage {
    let mut headless = WgpuHeadlessRenderer::new().expect("a headless renderer");
    headless
        .render_scene_to_image(scene, SIZE)
        .expect("the reference renders")
}

fn flipped(image: &image::RgbaImage) -> image::RgbaImage {
    image::imageops::flip_vertical(image)
}

fn inside(rect: Bounds<DevicePixels>, x: u32, y: u32) -> bool {
    let (x, y) = (x as i32, y as i32);
    x >= rect.origin.x.0
        && y >= rect.origin.y.0
        && x < rect.origin.x.0 + rect.size.width.0
        && y < rect.origin.y.0 + rect.size.height.0
}

#[test]
fn draws_like_the_surface_path() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    let scene = scene(gpui::red(), gpui::blue());
    let damage = harness.draw(&scene, false, 0);
    assert_eq!(
        damage,
        [device_rect(0, 0, 64, 48)],
        "a first frame is damaged whole"
    );
    assert_eq!(harness.read(), reference(&scene));
    Ok(())
}

#[test]
fn draws_upside_down_for_a_flipped_target() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    let scene = scene(gpui::red(), gpui::blue());
    harness.draw(&scene, true, 0);
    assert_eq!(harness.read(), flipped(&reference(&scene)));
    Ok(())
}

#[test]
fn partial() -> anyhow::Result<()> {
    for flip_y in [false, true] {
        let mut harness = Harness::new()?;
        harness.draw(&scene(gpui::red(), gpui::blue()), flip_y, 0);
        harness.mark();
        let next = scene(gpui::green(), gpui::blue());
        let damage = harness.draw(&next, flip_y, 1);
        assert_eq!(damage.len(), 1, "one block changed: {damage:?}");
        let rect = damage[0];
        assert!(
            rect.size.width.0 < 64 && rect.size.height.0 < 48,
            "{rect:?}"
        );
        let full = if flip_y {
            flipped(&reference(&next))
        } else {
            reference(&next)
        };
        // The damage is in the target's coordinates already.
        let image = harness.read();
        for (x, y, pixel) in image.enumerate_pixels() {
            if inside(rect, x, y) {
                assert_eq!(
                    pixel,
                    full.get_pixel(x, y),
                    "({x}, {y}) inside, flip {flip_y}"
                );
            } else {
                assert_eq!(pixel.0, MAGENTA, "({x}, {y}) outside kept, flip {flip_y}");
            }
        }
    }
    Ok(())
}

#[test]
fn an_unknown_age_or_an_old_target_redraws_everything() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    harness.draw(&scene(gpui::red(), gpui::blue()), false, 0);
    harness.mark();
    let next = scene(gpui::green(), gpui::blue());
    harness.draw(&next, false, 0);
    assert_eq!(harness.read(), reference(&next), "age 0");
    harness.mark();
    harness.draw(&next, false, 9);
    assert_eq!(harness.read(), reference(&next), "older than the history");
    Ok(())
}

#[test]
fn an_unchanged_frame_draws_nothing() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    let scene = scene(gpui::red(), gpui::blue());
    harness.draw(&scene, false, 0);
    harness.mark();
    assert!(harness.draw(&scene, false, 1).is_empty(), "no damage");
    assert!(
        harness.read().pixels().all(|p| p.0 == MAGENTA),
        "nothing drawn"
    );
    Ok(())
}

#[test]
fn an_older_target_gets_the_damage_of_every_frame_since() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    harness.draw(&scene(gpui::red(), gpui::blue()), false, 0);
    // The other block changes, then the first: a target two frames old needs both.
    let first = harness.draw(&scene(gpui::red(), gpui::yellow()), false, 1);
    harness.mark();
    let last = scene(gpui::green(), gpui::yellow());
    let second = harness.draw(&last, false, 2);
    let union = first[0].union(&second[0]);
    let full = reference(&last);
    let image = harness.read();
    for (x, y, pixel) in image.enumerate_pixels() {
        if inside(union, x, y) {
            assert_eq!(pixel, full.get_pixel(x, y), "({x}, {y}) inside");
        } else {
            assert_eq!(pixel.0, MAGENTA, "({x}, {y}) outside");
        }
    }
    harness.renderer.invalidate();
    harness.mark();
    harness.draw(&last, false, 1);
    assert_eq!(harness.read(), reference(&last), "after invalidate");
    Ok(())
}

/// Red over nothing: no opaque background under the block.
fn bare(block: Option<Hsla>) -> Scene {
    let mut scene = Scene::default();
    if let Some(color) = block {
        scene.insert_primitive(quad(16., 8., 16., 12., color));
    }
    scene.finish();
    scene
}

#[test]
fn a_partial_frame_clears_what_disappears() -> anyhow::Result<()> {
    for flip_y in [false, true] {
        let mut harness = Harness::new()?;
        harness.draw(&bare(Some(gpui::red())), flip_y, 0);
        let damage = harness.draw(&bare(None), flip_y, 1);
        assert_eq!(damage.len(), 1);
        let mut full = Harness::new()?;
        full.draw(&bare(None), flip_y, 0);
        assert_eq!(
            harness.read(),
            full.read(),
            "flip {flip_y}: no red left behind"
        );
    }
    Ok(())
}

#[test]
fn damage_is_given_before_drawing_in_target_coordinates() -> anyhow::Result<()> {
    let mut upright = Harness::new()?;
    let mut flipped = Harness::new()?;
    upright.frame(&scene(gpui::red(), gpui::blue()), false, 0);
    flipped.frame(&scene(gpui::red(), gpui::blue()), true, 0);
    let a = upright.frame(&scene(gpui::green(), gpui::blue()), false, 1);
    let b = flipped.frame(&scene(gpui::green(), gpui::blue()), true, 1);
    assert_eq!(
        a.redraw, a.damage,
        "a one-frame-old target redraws the damage"
    );
    assert_eq!(
        b.damage,
        vec![flip_rect(a.damage[0], 48)],
        "rows from the bottom"
    );
    assert_eq!(b.redraw, b.damage);
    Ok(())
}

#[test]
fn an_unchanged_frame_is_not_counted_in_the_age() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    harness.draw(&scene(gpui::red(), gpui::blue()), false, 0);
    // A: the first block changes. B: nothing (the caller may skip its swap).
    let a = harness.draw(&scene(gpui::green(), gpui::blue()), false, 1);
    let b = harness.frame(&scene(gpui::green(), gpui::blue()), false, 1);
    assert!(
        b.damage.is_empty() && b.redraw.is_empty(),
        "B draws nothing"
    );
    // C: the other block changes, into a target two frames old.
    harness.mark();
    let last = scene(gpui::green(), gpui::yellow());
    let c = harness.frame(&last, false, 2);
    assert!(
        c.redraw.iter().any(|r| r.union(&a[0]) == *r),
        "A's change is redrawn too: {c:?}"
    );
    let full = reference(&last);
    let image = harness.read();
    for (x, y, pixel) in image.enumerate_pixels() {
        if inside(a[0], x, y) {
            assert_eq!(pixel, full.get_pixel(x, y), "({x}, {y}) of A");
        }
    }
    Ok(())
}

#[test]
fn a_gpu_error_fails_the_next_frame_and_the_one_after_is_whole() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    let scene = scene(gpui::red(), gpui::blue());
    harness.draw(&scene, false, 0);
    // An invalid buffer: an uncaptured validation error on the device.
    let _bad = harness.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("invalid"),
        size: 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE,
        mapped_at_creation: false,
    });
    let failed = harness
        .renderer
        .draw(&scene, &harness.view, SIZE, false, 1, |_| {});
    assert!(failed.is_err(), "the error is reported");
    harness.mark();
    let damage = harness.draw(&scene, false, 1);
    assert_eq!(
        damage,
        [device_rect(0, 0, 64, 48)],
        "drawn whole after the failure"
    );
    assert_eq!(harness.read(), reference(&scene));
    Ok(())
}

#[test]
fn released_targets_are_freed_and_the_next_frame_is_whole() -> anyhow::Result<()> {
    let mut harness = Harness::new()?;
    let scene = scene(gpui::red(), gpui::blue());
    harness.draw(&scene, true, 0);
    assert!(
        harness
            .renderer
            .flip
            .as_ref()
            .is_some_and(|(_, source)| source.is_some()),
        "a flipped target keeps a texture"
    );
    assert!(
        harness
            .renderer
            .core
            .resources
            .path_intermediate_view
            .is_some()
    );

    harness.renderer.release_targets();
    assert!(
        harness
            .renderer
            .flip
            .as_ref()
            .is_none_or(|(_, source)| source.is_none()),
        "the flip texture is freed"
    );
    assert!(
        harness
            .renderer
            .core
            .resources
            .path_intermediate_view
            .is_none()
    );
    assert!(harness.renderer.core.resources.path_msaa_view.is_none());

    harness.mark();
    let damage = harness.draw(&scene, true, 1);
    assert_eq!(damage, [device_rect(0, 0, 64, 48)], "drawn whole");
    assert_eq!(harness.read(), flipped(&reference(&scene)));
    Ok(())
}

#[test]
fn dynamic_textures_are_made_from_the_renderers_context() -> anyhow::Result<()> {
    let harness = Harness::new()?;
    let context = harness.renderer.context();
    assert!(*context.device == harness.device, "the caller's device");
    let texture = context.create_dynamic_texture(size(DevicePixels(8), DevicePixels(8)))?;
    drop(texture);
    Ok(())
}
