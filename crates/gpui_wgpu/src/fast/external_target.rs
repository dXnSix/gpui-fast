//! Drawing into a target the caller supplies, with partial redraws.
//!
//! [`WgpuExternalRenderer`] draws GPUI scenes on a wgpu device and adapter the
//! caller created (one over a GL context the platform owns, through
//! `wgpu_hal::gles::Adapter::new_external`, for example) into a texture view
//! the caller supplies for each frame. No `wgpu::Surface` is created or
//! presented: the caller presents.
//!
//! Each frame's damage comes from [`crate::fast::damage`]. Given the target's
//! age (how many frames ago its contents were drawn, 0 when unknown, as
//! `EGL_EXT_buffer_age` reports it), the renderer loads the target's contents,
//! clears the bounding rectangle of the damage of the last `age` frames and
//! draws only inside it; age 0, an age beyond the kept history, or a frame
//! damaged whole draws everything. The caller gets the damage before anything
//! is drawn, in the target's coordinates. A Y-flipped target (a GL default framebuffer) is
//! drawn through a texture the renderer keeps, which then always holds the
//! previous frame: the frame draws its own damage there, and the target's
//! region is copied into the target upside down.
//!
//! This is the fork's second public API exception, listed in
//! `docs/upstream-sync.md`.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use gpui::{Bounds, DevicePixels, Scene, Size, point};

use crate::WgpuAtlas;
use crate::fast::damage::Damage;
use crate::wgpu_context::WgpuContext;
use crate::wgpu_renderer::WgpuRendererCore;

/// How many frames of damage are kept: targets older than this are redrawn
/// whole.
pub(crate) const HISTORY: usize = 4;

/// What a frame draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Region {
    /// Everything (cleared first).
    Full,
    /// Only inside this rectangle, keeping the rest.
    Partial(Bounds<DevicePixels>),
    /// Nothing: the target is up to date.
    Nothing,
}

/// The region to draw into a target of `age` frames, given the damage of the
/// last frames, newest first (`None` for a frame damaged whole).
pub(crate) fn redraw_region(
    history: &VecDeque<Option<Vec<Bounds<DevicePixels>>>>,
    age: u32,
    size: Size<DevicePixels>,
) -> Region {
    let age = age as usize;
    if age == 0 || age > history.len() {
        return Region::Full;
    }
    let mut union: Option<Bounds<DevicePixels>> = None;
    for frame in history.iter().take(age) {
        let Some(rects) = frame else {
            return Region::Full;
        };
        for rect in rects {
            union = Some(union.map_or(*rect, |u| u.union(rect)));
        }
    }
    match union {
        None => Region::Nothing,
        Some(rect)
            if rect.origin.x.0 <= 0
                && rect.origin.y.0 <= 0
                && rect.size.width.0 >= size.width.0
                && rect.size.height.0 >= size.height.0 =>
        {
            Region::Full
        }
        Some(rect) => Region::Partial(rect),
    }
}

/// `rect` of a target of `height` pixels, mirrored top to bottom.
pub(crate) fn flip_rect(rect: Bounds<DevicePixels>, height: i32) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(
            rect.origin.x,
            DevicePixels(height - rect.origin.y.0 - rect.size.height.0),
        ),
        size: rect.size,
    }
}

/// The texture a Y-flipped target is drawn through.
struct FlipSource {
    size: Size<DevicePixels>,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// Copies a texture into the target upside down.
struct FlipBlit {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

const FLIP_SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = textureDimensions(source);
    let x = u32(position.x);
    let y = size.y - 1u - u32(position.y);
    return textureLoad(source, vec2<u32>(x, y), 0);
}
"#;

impl FlipBlit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flip_blit_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flip_blit"),
            source: wgpu::ShaderSource::Wgsl(FLIP_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flip_blit_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flip_blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { layout, pipeline }
    }
}

/// Fills the scissored part of a target with transparent black, as a full
/// frame's clear would: a partial frame is drawn over the cleared rectangle.
struct ClearRect {
    pipeline: wgpu::RenderPipeline,
}

const CLEAR_SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
"#;

impl ClearRect {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clear_rect"),
            source: wgpu::ShaderSource::Wgsl(CLEAR_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("clear_rect"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clear_rect"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline }
    }
}

/// Draws a full-screen triangle into `view` inside `rect` only, keeping the
/// rest of the target.
fn scissored_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    rect: Bounds<DevicePixels>,
    label: &str,
    draw: impl FnOnce(&mut wgpu::RenderPass<'_>),
) {
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(
            rect.origin.x.0 as u32,
            rect.origin.y.0 as u32,
            rect.size.width.0 as u32,
            rect.size.height.0 as u32,
        );
        draw(&mut pass);
    }
    queue.submit(std::iter::once(encoder.finish()));
}

fn whole(size: Size<DevicePixels>) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(DevicePixels(0), DevicePixels(0)),
        size,
    }
}

fn region_rects(region: Region, size: Size<DevicePixels>) -> Vec<Bounds<DevicePixels>> {
    match region {
        Region::Full => vec![whole(size)],
        Region::Partial(rect) => vec![rect],
        Region::Nothing => Vec::new(),
    }
}

/// A frame's damage, in the target's coordinates: with `flip_y`, rows count
/// from the bottom, as EGL's damage rectangles do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WgpuFrameDamage {
    /// Where this frame's pixels differ from the previous frame's (for
    /// `eglSwapBuffersWithDamageKHR`); the whole target for a frame damaged
    /// whole, none when nothing changed.
    pub damage: Vec<Bounds<DevicePixels>>,
    /// What the frame draws into the target (for `eglSetDamageRegionKHR`):
    /// the region its age asks for; none when the target is up to date.
    pub redraw: Vec<Bounds<DevicePixels>>,
}

/// A GPUI renderer that draws into texture views the caller supplies, on a
/// device the caller created, redrawing only what changed when the target
/// keeps its contents.
pub struct WgpuExternalRenderer {
    context: WgpuContext,
    core: WgpuRendererCore,
    damage: crate::fast::damage::DamageTracker,
    /// The damage of the last frames that had any, newest first; `None` for
    /// a frame damaged whole. Frames without damage are not recorded, so an
    /// age counted in presented frames covers at least what it needs.
    history: VecDeque<Option<Vec<Bounds<DevicePixels>>>>,
    clear: Option<ClearRect>,
    flip: Option<(FlipBlit, Option<FlipSource>)>,
    observed_error_generation: u64,
}

impl WgpuExternalRenderer {
    /// A renderer on the caller's `device` and `queue` (created from `adapter`
    /// of `instance`), drawing into views of `format`.
    ///
    /// The renderer installs the device's lost and uncaptured-error callbacks
    /// (replacing any the caller set, and wgpu's default panic); [`draw`]
    /// reports those errors.
    ///
    /// [`draw`]: Self::draw
    pub fn new(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        let color_texture_format = WgpuContext::select_color_texture_format(&adapter)?;
        let dual_source_blending = device
            .features()
            .contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        let context = WgpuContext::from_native_adapter_and_device(
            instance,
            adapter,
            device,
            queue,
            dual_source_blending,
            color_texture_format,
        );
        let observed_error_generation = context.errors().current_generation();
        let atlas = Arc::new(WgpuAtlas::from_context(&context));
        let core = WgpuRendererCore::new(&context, atlas, format, wgpu::CompositeAlphaMode::Opaque);
        Ok(Self {
            context,
            core,
            damage: crate::fast::damage::DamageTracker::default(),
            history: VecDeque::with_capacity(HISTORY + 1),
            clear: None,
            flip: None,
            observed_error_generation,
        })
    }

    /// The sprite atlas the window's text and images are rasterized into.
    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        &self.core.atlas
    }

    /// The context the renderer draws with: dynamic textures painted into
    /// its frames are made from it (`WgpuContext::create_dynamic_texture`).
    pub fn context(&self) -> &WgpuContext {
        &self.context
    }

    /// The next frame is drawn whole (the target's contents or the atlas were
    /// lost).
    pub fn invalidate(&mut self) {
        self.damage.invalidate();
        self.history.clear();
        if let Some((_, source)) = &mut self.flip {
            *source = None;
        }
    }

    /// Frees the target-sized textures the renderer keeps (the app is paused,
    /// its surface released): the texture a flipped target is drawn through
    /// and the path intermediates. The next frame makes them again and is
    /// drawn whole.
    pub fn release_targets(&mut self) {
        self.invalidate();
        self.core.resources.invalidate_intermediate_textures();
    }

    /// Draws `scene` into `target`, a view of a `size` texture whose contents
    /// were drawn `age` frames ago (0: unknown), upside down when `flip_y`.
    /// `before_drawing` gets the frame's damage before anything is drawn, for
    /// `eglSetDamageRegionKHR`; the same is returned, for
    /// `eglSwapBuffersWithDamageKHR`.
    ///
    /// A GPU error raised since the previous frame fails this frame; after any
    /// failure the next frame is drawn whole.
    pub fn draw(
        &mut self,
        scene: &Scene,
        target: &wgpu::TextureView,
        size: Size<DevicePixels>,
        flip_y: bool,
        age: u32,
        before_drawing: impl FnOnce(&WgpuFrameDamage),
    ) -> Result<WgpuFrameDamage> {
        if self.context.device_lost() {
            self.invalidate();
            anyhow::bail!("the GPU device was lost");
        }
        if let Some(error) = self
            .context
            .errors()
            .observe_error(&mut self.observed_error_generation)
        {
            self.invalidate();
            anyhow::bail!("GPU error during an earlier frame: {error}");
        }
        let result = self.draw_frame(scene, target, size, flip_y, age, before_drawing);
        if result.is_err() {
            self.invalidate();
        }
        result
    }

    fn draw_frame(
        &mut self,
        scene: &Scene,
        target: &wgpu::TextureView,
        size: Size<DevicePixels>,
        flip_y: bool,
        age: u32,
        before_drawing: impl FnOnce(&WgpuFrameDamage),
    ) -> Result<WgpuFrameDamage> {
        let damage = self.damage.frame(scene, size);
        let frame_rects = damage.rects(size);
        // What this frame changes, for a target that holds the previous frame.
        let own = match &damage {
            Damage::Full => Region::Full,
            Damage::Rects(rects) if rects.is_empty() => Region::Nothing,
            Damage::Rects(rects) => {
                let mut history = VecDeque::new();
                history.push_front(Some(rects.clone()));
                redraw_region(&history, 1, size)
            }
        };
        match damage {
            Damage::Full => self.history.push_front(None),
            Damage::Rects(rects) if !rects.is_empty() => self.history.push_front(Some(rects)),
            Damage::Rects(_) => {}
        }
        self.history.truncate(HISTORY);
        // What the target needs: an unchanged frame owes it only the frames before.
        let wanted = match (age, own) {
            (0, _) => Region::Full,
            (1, Region::Nothing) => Region::Nothing,
            (age, Region::Nothing) => redraw_region(&self.history, age - 1, size),
            (age, _) => redraw_region(&self.history, age, size),
        };

        let device = Arc::clone(&self.core.resources.device);
        let format = self.core.target_format;
        let fresh = flip_y
            && self
                .flip
                .as_ref()
                .and_then(|(_, source)| source.as_ref())
                .is_none_or(|source| source.size != size);
        let (own, wanted) = if fresh {
            (Region::Full, Region::Full)
        } else {
            (own, wanted)
        };
        let to_target = |rects: Vec<Bounds<DevicePixels>>| -> Vec<Bounds<DevicePixels>> {
            if flip_y {
                rects
                    .into_iter()
                    .map(|rect| flip_rect(rect, size.height.0))
                    .collect()
            } else {
                rects
            }
        };
        let frame_damage = WgpuFrameDamage {
            damage: to_target(frame_rects),
            redraw: to_target(region_rects(wanted, size)),
        };
        before_drawing(&frame_damage);

        if !flip_y {
            self.flip = None;
            self.draw_region(scene, target, size, wanted)?;
            return Ok(frame_damage);
        }

        // A flipped target is drawn through a texture the renderer keeps, which
        // holds the previous frame unless it was just made.
        let (blit, source) = self
            .flip
            .get_or_insert_with(|| (FlipBlit::new(&device, format), None));
        if fresh {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("flip_source"),
                size: wgpu::Extent3d {
                    width: size.width.0 as u32,
                    height: size.height.0 as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flip_source"),
                layout: &blit.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            *source = Some(FlipSource {
                size,
                view,
                bind_group,
            });
        }
        let view = source.as_ref().map(|s| s.view.clone()).expect("made above");
        self.draw_region(scene, &view, size, own)?;
        let copy = match wanted {
            Region::Nothing => return Ok(frame_damage),
            Region::Partial(rect) => flip_rect(rect, size.height.0),
            Region::Full => whole(size),
        };
        let (blit, source) = self.flip.as_ref().expect("made above");
        let source = source.as_ref().expect("made above");
        scissored_pass(
            &device,
            &self.core.resources.queue,
            target,
            copy,
            "flip_blit",
            |pass| {
                pass.set_pipeline(&blit.pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
                pass.draw(0..3, 0..1);
            },
        );
        Ok(frame_damage)
    }

    fn draw_region(
        &mut self,
        scene: &Scene,
        view: &wgpu::TextureView,
        size: Size<DevicePixels>,
        region: Region,
    ) -> Result<()> {
        let partial = match region {
            Region::Nothing => return Ok(()),
            Region::Full => None,
            Region::Partial(rect) => {
                // A full frame starts from a cleared target; so does the rectangle.
                let device = Arc::clone(&self.core.resources.device);
                let format = self.core.target_format;
                let clear = self
                    .clear
                    .get_or_insert_with(|| ClearRect::new(&device, format));
                scissored_pass(
                    &device,
                    &self.core.resources.queue,
                    view,
                    rect,
                    "clear_rect",
                    |pass| {
                        pass.set_pipeline(&clear.pipeline);
                        pass.draw(0..3, 0..1);
                    },
                );
                Some(rect)
            }
        };
        self.core.fast_frame.partial = partial;
        let result = self
            .core
            .render_frame(scene, view, size, false, wgpu::Color::TRANSPARENT);
        self.core.fast_frame.partial = None;
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests;
