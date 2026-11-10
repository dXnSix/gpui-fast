// Modified by Longbridge for gpui-fast.
//! Scene damage: the device-pixel rectangles of a window whose pixels can
//! differ between two consecutive frames, for renderers that keep the last
//! frame's pixels and redraw only what changed (partial redraw).
//!
//! A scene is summarized as its primitives in paint order (the order of
//! `Scene::batches`), each one a hash of everything that decides its pixels
//! except its draw order, and the area it can touch: its bounds clipped by its
//! content mask, widened for blur, wavy underlines and antialiasing. Two
//! summaries are compared with a shortest-edit-script diff. Primitives on the
//! common subsequence keep their content and their relative paint order, so a
//! pixel only they cover is unchanged; the damage is the area of every other
//! primitive, removed or added. The rectangles are clamped to the window and
//! merged into at most [`MAX_RECTS`].
//!
//! Damage is conservative: when in doubt, more is redrawn, never less. The
//! whole window is damaged on the first frame, after a resize, after
//! [`DamageTracker::invalidate`] (scale change, atlas loss, device recovery),
//! when the scenes differ by more than [`MAX_EDITS`] primitives, or always
//! with `GPUI_PARTIAL_REDRAW=0`. Surfaces, whose pixels change while their
//! description does not, are damaged every frame; scroll-layer tiles change
//! with their layer's generation, rounded clip groups with their content and
//! geometry; sprites with a non-identity transform cover the whole window.
//! More than [`MAX_MERGED_AREAS`] changed areas become one rectangle.

use std::hash::{DefaultHasher, Hasher};

use gpui::{
    Bounds, DevicePixels, LAYER_TILE_TEXTURE_BASE, PrimitiveBatch, ScaledPixels, Scene, Size,
    TransformationMatrix, decode_layer_tile, point,
};

/// The most rectangles a damage holds; more are merged.
pub(crate) const MAX_RECTS: usize = 8;
/// Past this many changed areas they are not merged pairwise (cubic in their
/// count) but into one bounding rectangle.
pub(crate) const MAX_MERGED_AREAS: usize = 64;
/// Past this many added and removed primitives the diff stops and the whole
/// window is damaged.
pub(crate) const MAX_EDITS: usize = 512;
/// `GPUI_PARTIAL_REDRAW=0` turns partial redraw off: every frame is damaged
/// whole.
pub(crate) const ENV: &str = "GPUI_PARTIAL_REDRAW";
/// Antialiasing reaches about a pixel past a primitive's bounds.
const AA_MARGIN: f32 = 1.;

/// What changed between two frames.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Damage {
    /// The whole window.
    Full,
    /// These rectangles, inside the window and at most [`MAX_RECTS`]; none
    /// when the frames look the same.
    Rects(Vec<Bounds<DevicePixels>>),
}

impl Damage {
    /// The damaged rectangles of a window of `size`: the whole window for
    /// [`Damage::Full`].
    pub(crate) fn rects(&self, size: Size<DevicePixels>) -> Vec<Bounds<DevicePixels>> {
        match self {
            Damage::Full => vec![Bounds {
                origin: point(DevicePixels(0), DevicePixels(0)),
                size,
            }],
            Damage::Rects(rects) => rects.clone(),
        }
    }
}

/// One primitive of a scene summary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Item {
    hash: u64,
    /// The area the primitive can touch, in device pixels; `None` for the
    /// whole window.
    area: Option<Bounds<ScaledPixels>>,
}

/// A window's damage from frame to frame.
pub(crate) struct DamageTracker {
    previous: Option<Vec<Item>>,
    size: Option<Size<DevicePixels>>,
    enabled: bool,
    /// Counts frames, for primitives damaged every frame.
    frame: u64,
}

impl Default for DamageTracker {
    fn default() -> Self {
        Self::new(std::env::var(ENV).map_or(true, |value| value.trim() != "0"))
    }
}

impl DamageTracker {
    /// A tracker that computes damage, or with `enabled` false damages every
    /// frame whole.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            previous: None,
            size: None,
            enabled,
            frame: 0,
        }
    }

    /// The next frame is damaged whole (scale change, atlas loss, a target
    /// whose contents were lost).
    pub(crate) fn invalidate(&mut self) {
        self.previous = None;
    }

    /// The damage of drawing `scene` into a window of `size` after the
    /// previous frame this tracker saw.
    pub(crate) fn frame(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Damage {
        self.frame = self.frame.wrapping_add(1);
        let items = summarize(scene, self.frame);
        let previous = self.previous.replace(items);
        let resized = self.size.replace(size) != Some(size);
        if !self.enabled || resized {
            return Damage::Full;
        }
        let Some(previous) = previous else {
            return Damage::Full;
        };
        let current = self.previous.as_deref().unwrap_or_default();
        match changed_areas(&previous, current) {
            Some(areas) => merge(areas, size),
            None => Damage::Full,
        }
    }
}

fn hash_bytes<T: Copy>(hasher: &mut impl Hasher, value: &T) {
    // SAFETY: the scene's primitives are plain `#[repr(C)]` data uploaded to the GPU as they
    // are; reading their bytes is how the renderer uploads them. Padding the compiler may
    // insert only makes equal primitives look different, which over-damages.
    let bytes = unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    };
    hasher.write(bytes);
}

fn clipped(
    bounds: Bounds<ScaledPixels>,
    mask: Bounds<ScaledPixels>,
    margin: f32,
) -> Bounds<ScaledPixels> {
    bounds
        .dilate(ScaledPixels(margin + AA_MARGIN))
        .intersect(&mask.dilate(ScaledPixels(AA_MARGIN)))
}

/// `scene` as paint-ordered [`Item`]s. `frame` salts the hashes of
/// primitives damaged every frame.
pub(crate) fn summarize(scene: &Scene, frame: u64) -> Vec<Item> {
    let mut items = Vec::new();
    summarize_into(scene, frame, &mut items);
    items
}

fn summarize_into(scene: &Scene, frame: u64, items: &mut Vec<Item>) {
    for batch in scene.batches() {
        match batch {
            PrimitiveBatch::Shadows(range) => {
                for shadow in &scene.shadows[range] {
                    let mut key = *shadow;
                    key.order = 0;
                    let mut hasher = item_hasher(0);
                    hash_bytes(&mut hasher, &key);
                    // A Gaussian blur reaches about three radii out.
                    let margin = 3. * shadow.blur_radius.0.max(0.);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(shadow.bounds, shadow.content_mask.bounds, margin)),
                    });
                }
            }
            PrimitiveBatch::Quads(range) => {
                for quad in &scene.quads[range] {
                    let mut key = *quad;
                    key.order = 0;
                    let mut hasher = item_hasher(1);
                    hash_bytes(&mut hasher, &key);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(quad.bounds, quad.content_mask.bounds, 0.)),
                    });
                }
            }
            PrimitiveBatch::Paths(range) => {
                for path in &scene.paths[range] {
                    let mut hasher = item_hasher(2);
                    hash_bytes(&mut hasher, &path.bounds);
                    hash_bytes(&mut hasher, &path.content_mask.bounds);
                    hash_bytes(&mut hasher, &path.color);
                    hasher.write_usize(path.vertices.len());
                    for vertex in &path.vertices {
                        hash_bytes(&mut hasher, &vertex.xy_position);
                        hash_bytes(&mut hasher, &vertex.st_position);
                        hash_bytes(&mut hasher, &vertex.content_mask.bounds);
                    }
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(path.bounds, path.content_mask.bounds, 0.)),
                    });
                }
            }
            PrimitiveBatch::Underlines(range) => {
                for underline in &scene.underlines[range] {
                    let mut key = *underline;
                    key.order = 0;
                    let mut hasher = item_hasher(3);
                    hash_bytes(&mut hasher, &key);
                    // A wavy underline swings by its thickness.
                    let margin = underline.thickness.0.max(0.);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(
                            underline.bounds,
                            underline.content_mask.bounds,
                            margin,
                        )),
                    });
                }
            }
            PrimitiveBatch::MonochromeSprites { range, .. } => {
                for sprite in &scene.monochrome_sprites[range] {
                    let mut key = *sprite;
                    key.order = 0;
                    let mut hasher = item_hasher(4);
                    hash_bytes(&mut hasher, &key);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: sprite_area(
                            sprite.bounds,
                            sprite.content_mask.bounds,
                            sprite.transformation,
                        ),
                    });
                }
            }
            PrimitiveBatch::SubpixelSprites { range, .. } => {
                for sprite in &scene.subpixel_sprites[range] {
                    let mut key = *sprite;
                    key.order = 0;
                    let mut hasher = item_hasher(5);
                    hash_bytes(&mut hasher, &key);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: sprite_area(
                            sprite.bounds,
                            sprite.content_mask.bounds,
                            sprite.transformation,
                        ),
                    });
                }
            }
            PrimitiveBatch::PolychromeSprites { range, .. } => {
                for sprite in &scene.polychrome_sprites[range] {
                    let mut key = *sprite;
                    key.order = 0;
                    let mut hasher = item_hasher(6);
                    hash_bytes(&mut hasher, &key);
                    hash_sprite_source(&mut hasher, scene, sprite, frame);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(sprite.bounds, sprite.content_mask.bounds, 0.)),
                    });
                }
            }
            PrimitiveBatch::Surfaces(range) => {
                for surface in &scene.surfaces[range] {
                    // A surface's content changes without the scene saying so.
                    let mut hasher = item_hasher(7);
                    hasher.write_u64(frame);
                    items.push(Item {
                        hash: hasher.finish(),
                        area: Some(clipped(surface.bounds, surface.content_mask.bounds, 0.)),
                    });
                }
            }
        }
    }
}

fn item_hasher(kind: u8) -> DefaultHasher {
    let mut hasher = DefaultHasher::new();
    hasher.write_u8(kind);
    hasher
}

fn sprite_area(
    bounds: Bounds<ScaledPixels>,
    mask: Bounds<ScaledPixels>,
    transformation: TransformationMatrix,
) -> Option<Bounds<ScaledPixels>> {
    (transformation == TransformationMatrix::unit()).then(|| clipped(bounds, mask, 0.))
}

/// What a polychrome sprite draws besides its atlas tile: a scroll layer's
/// tile (its layer's generation, and every frame while the tile is dirty) or
/// a rounded clip group's content scene.
fn hash_sprite_source(
    hasher: &mut DefaultHasher,
    scene: &Scene,
    sprite: &gpui::PolychromeSprite,
    frame: u64,
) {
    let texture = sprite.tile.texture_id;
    if let Some(group) = crate::fast::rounded_clip::group(scene, texture) {
        if let Some(content) = group.content.scene() {
            for item in summarize(content, frame) {
                hasher.write_u64(item.hash);
            }
            // The group's geometry frame (its vertical radii are in its
            // background) and any layers inside it.
            for layer in &content.layers.frames {
                hasher.write_u32(layer.key.0);
                hasher.write_u64(layer.generation);
                hasher.write_u32(layer.tile_size);
                hash_bytes(hasher, &layer.background);
            }
        } else {
            hasher.write_u64(frame);
        }
        return;
    }
    if texture.index < LAYER_TILE_TEXTURE_BASE {
        return;
    }
    match decode_layer_tile(texture, sprite.tile.tile_id) {
        // The layer's generation changes with its content.
        Some((layer, _tile)) => match scene.layers.frames.iter().find(|frame| frame.key == layer) {
            Some(layer_frame) => hasher.write_u64(layer_frame.generation),
            None => hasher.write_u64(frame),
        },
        None => hasher.write_u64(frame),
    }
}

/// The areas of the primitives not on the longest common subsequence of
/// `previous` and `current`, or `None` when they differ by more than
/// [`MAX_EDITS`] primitives (or one covers the whole window).
pub(crate) fn changed_areas(
    previous: &[Item],
    current: &[Item],
) -> Option<Vec<Bounds<ScaledPixels>>> {
    let prefix = previous
        .iter()
        .zip(current)
        .take_while(|(a, b)| a.hash == b.hash)
        .count();
    let previous = &previous[prefix..];
    let current = &current[prefix..];
    let suffix = previous
        .iter()
        .rev()
        .zip(current.iter().rev())
        .take_while(|(a, b)| a.hash == b.hash)
        .count();
    let previous = &previous[..previous.len() - suffix];
    let current = &current[..current.len() - suffix];
    let a: Vec<u64> = previous.iter().map(|item| item.hash).collect();
    let b: Vec<u64> = current.iter().map(|item| item.hash).collect();
    let (kept_a, kept_b) = diff(&a, &b, MAX_EDITS)?;
    let mut areas = Vec::new();
    for (item, kept) in previous
        .iter()
        .zip(kept_a)
        .chain(current.iter().zip(kept_b))
    {
        if !kept {
            areas.push(item.area?);
        }
    }
    Some(areas)
}

/// Myers' shortest edit script between `a` and `b`: for each element of
/// either, whether it is on the longest common subsequence. `None` when more
/// than `max_edits` insertions and deletions are needed.
pub(crate) fn diff(a: &[u64], b: &[u64], max_edits: usize) -> Option<(Vec<bool>, Vec<bool>)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m).min(max_edits as isize);
    let offset = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let d_final = found?;
    let mut kept_a = vec![false; a.len()];
    let mut kept_b = vec![false; b.len()];
    let (mut x, mut y) = (n, m);
    // `trace[d]` holds the furthest x per diagonal before step `d`.
    for d in (1..=d_final).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let at = |k: isize| v[(k + offset) as usize];
        let previous_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = at(previous_k);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            kept_a[x as usize] = true;
            kept_b[y as usize] = true;
        }
        x = previous_x;
        y = previous_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        kept_a[x as usize] = true;
        kept_b[y as usize] = true;
    }
    Some((kept_a, kept_b))
}

fn to_device(
    bounds: Bounds<ScaledPixels>,
    size: Size<DevicePixels>,
) -> Option<Bounds<DevicePixels>> {
    let left = (bounds.origin.x.0.floor() as i32).clamp(0, size.width.0);
    let top = (bounds.origin.y.0.floor() as i32).clamp(0, size.height.0);
    let right = ((bounds.origin.x.0 + bounds.size.width.0).ceil() as i32).clamp(0, size.width.0);
    let bottom = ((bounds.origin.y.0 + bounds.size.height.0).ceil() as i32).clamp(0, size.height.0);
    (right > left && bottom > top).then(|| Bounds {
        origin: point(DevicePixels(left), DevicePixels(top)),
        size: gpui::size(DevicePixels(right - left), DevicePixels(bottom - top)),
    })
}

fn area(bounds: &Bounds<DevicePixels>) -> i64 {
    i64::from(bounds.size.width.0) * i64::from(bounds.size.height.0)
}

fn union(a: &Bounds<DevicePixels>, b: &Bounds<DevicePixels>) -> Bounds<DevicePixels> {
    a.union(b)
}

fn touch(a: &Bounds<DevicePixels>, b: &Bounds<DevicePixels>) -> bool {
    a.origin.x.0 <= b.origin.x.0 + b.size.width.0
        && b.origin.x.0 <= a.origin.x.0 + a.size.width.0
        && a.origin.y.0 <= b.origin.y.0 + b.size.height.0
        && b.origin.y.0 <= a.origin.y.0 + a.size.height.0
}

/// `areas` as device-pixel rectangles inside a window of `size`: those that
/// overlap or touch merged, then the pairs whose union adds the least area
/// merged until at most [`MAX_RECTS`] remain. The whole window when they
/// cover it.
pub(crate) fn merge(areas: Vec<Bounds<ScaledPixels>>, size: Size<DevicePixels>) -> Damage {
    let mut rects: Vec<Bounds<DevicePixels>> = areas
        .into_iter()
        .filter_map(|a| to_device(a, size))
        .collect();
    if rects.len() > MAX_MERGED_AREAS {
        let bounding = rects.iter().skip(1).fold(rects[0], |all, r| union(&all, r));
        rects = vec![bounding];
    }
    // Overlapping or touching rectangles become one.
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if touch(&rects[i], &rects[j]) {
                    let other = rects.swap_remove(j);
                    rects[i] = union(&rects[i], &other);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    while rects.len() > MAX_RECTS {
        let mut best = (0, 1, i64::MAX);
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let grown = area(&union(&rects[i], &rects[j])) - area(&rects[i]) - area(&rects[j]);
                if grown < best.2 {
                    best = (i, j, grown);
                }
            }
        }
        let other = rects.swap_remove(best.1);
        rects[best.0] = union(&rects[best.0], &other);
    }
    let window = i64::from(size.width.0) * i64::from(size.height.0);
    if rects.len() == 1 && area(&rects[0]) >= window {
        return Damage::Full;
    }
    rects.sort_by_key(|r| (r.origin.y.0, r.origin.x.0));
    Damage::Rects(rects)
}

#[cfg(test)]
mod tests;
