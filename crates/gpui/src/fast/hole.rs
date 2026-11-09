//! Holes: rounded rectangles that replace the window's pixels with
//! transparent black instead of blending over them, so that what lies under
//! the window shows through — on a TV, the hardware video plane below the UI
//! plane. A video element paints a hole where the video plays; the platform
//! makes its window translucent while the frame has one
//! ([`Scene::has_holes`]).
//!
//! A hole is a polychrome sprite whose tile carries a reserved atlas texture
//! id ([`AtlasTextureId::hole`]), so it gets a batch of its own in the
//! scene's draw order without a new primitive kind: what is painted after it
//! blends over it as usual. Renderers that draw holes recognize the id
//! ([`AtlasTextureId::is_hole`]); the others find no such texture and skip
//! the batch, as they do for a released atlas texture. Only the wgpu renderer
//! draws holes.
//!
//! A hole inside a scroll layer's cached tiles clears those tile pixels; it
//! does not reach the window.

use crate::{
    AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, Corners, DevicePixels, Pixels,
    PolychromeSprite, ScaledPixels, Scene, TileId, Window, point, size,
};

/// The atlas texture index of holes, below the dynamic textures' range
/// (`0xE000_0000`), the layer tiles' (`0xF000_0000`) and the rounded clip
/// groups' (`0xF100_0000`).
pub(crate) const HOLE_TEXTURE_INDEX: u32 = 0xDFFF_FFFF;

impl AtlasTextureId {
    /// The texture id that marks a polychrome sprite as a hole.
    pub const fn hole() -> Self {
        Self {
            index: HOLE_TEXTURE_INDEX,
            kind: AtlasTextureKind::Polychrome,
        }
    }

    /// Whether a sprite with this texture id is a hole.
    pub fn is_hole(&self) -> bool {
        *self == Self::hole()
    }
}

impl Scene {
    /// Whether the scene paints a hole, directly or inside a layer or rounded
    /// clip group it carries.
    pub fn has_holes(&self) -> bool {
        self.polychrome_sprites
            .iter()
            .any(|sprite| sprite.tile.texture_id.is_hole())
            || self
                .layers
                .frames
                .iter()
                .filter_map(|frame| frame.content.scene())
                .any(Scene::has_holes)
    }
}

/// The sprite of a hole at `bounds` with `corner_radii`, in device pixels.
pub(crate) fn sprite(
    bounds: Bounds<ScaledPixels>,
    content_mask: crate::ContentMask<ScaledPixels>,
    corner_radii: Corners<ScaledPixels>,
) -> PolychromeSprite {
    PolychromeSprite {
        order: 0,
        pad: 0,
        tile: AtlasTile {
            texture_id: AtlasTextureId::hole(),
            tile_id: TileId(0),
            padding: 0,
            bounds: Bounds {
                origin: point(DevicePixels(0), DevicePixels(0)),
                size: size(DevicePixels(0), DevicePixels(0)),
            },
        },
        grayscale: false.into(),
        opacity: 1.0,
        bounds,
        content_mask,
        corner_radii,
    }
}

/// `bounds` scaled and rounded to whole device pixels, the grid the video
/// plane is positioned on.
pub(crate) fn snapped(bounds: Bounds<Pixels>, scale: f32) -> Bounds<ScaledPixels> {
    let left = (bounds.origin.x.0 * scale).round();
    let top = (bounds.origin.y.0 * scale).round();
    let right = ((bounds.origin.x.0 + bounds.size.width.0) * scale).round();
    let bottom = ((bounds.origin.y.0 + bounds.size.height.0) * scale).round();
    Bounds {
        origin: point(ScaledPixels(left), ScaledPixels(top)),
        size: size(
            ScaledPixels((right - left).max(0.)),
            ScaledPixels((bottom - top).max(0.)),
        ),
    }
}

impl Window {
    /// Paints a hole at `bounds`: the window's pixels there become
    /// transparent black, corners rounded by `corner_radii`, within the
    /// current content mask. What is painted after it blends over it.
    ///
    /// Only the wgpu renderer draws holes; a platform shows what lies under a
    /// hole only while its window is translucent.
    pub fn paint_hole(&mut self, bounds: Bounds<Pixels>, corner_radii: Corners<Pixels>) {
        self.invalidator.debug_assert_paint();
        if bounds.size.width <= Pixels::ZERO || bounds.size.height <= Pixels::ZERO {
            return;
        }
        let scale = self.scale_factor();
        let corner_radii = corner_radii
            .clamp_radii_for_quad_size(bounds.size)
            .scale(scale);
        let content_mask = self.snapped_content_mask();
        self.next_frame.scene.insert_primitive(sprite(
            snapped(bounds, scale),
            content_mask,
            corner_radii,
        ));
    }
}
