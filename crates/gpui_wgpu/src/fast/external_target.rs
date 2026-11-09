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
//! `EGL_EXT_buffer_age` reports it), the renderer loads the target's contents
//! and draws only inside the bounding rectangle of the damage of the last
//! `age` frames; age 0, an age beyond the kept history, or a frame damaged
//! whole draws everything. A Y-flipped target (a GL default framebuffer) is
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

/// A GPUI renderer that draws into texture views the caller supplies, on a
/// device the caller created, redrawing only what changed when the target
/// keeps its contents.
pub struct WgpuExternalRenderer {
    context: WgpuContext,
    core: WgpuRendererCore,
    /// The damage of the last frames, newest first; `None` for a frame
    /// damaged whole.
    history: VecDeque<Option<Vec<Bounds<DevicePixels>>>>,
    flip: Option<(FlipBlit, Option<FlipSource>)>,
}

impl WgpuExternalRenderer {
    /// A renderer on the caller's `device` and `queue` (created from `adapter`
    /// of `instance`), drawing into views of `format`.
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
        let atlas = Arc::new(WgpuAtlas::from_context(&context));
        let core = WgpuRendererCore::new(&context, atlas, format, wgpu::CompositeAlphaMode::Opaque);
        Ok(Self {
            context,
            core,
            history: VecDeque::with_capacity(HISTORY + 1),
            flip: None,
        })
    }

    /// The sprite atlas the window's text and images are rasterized into.
    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        &self.core.atlas
    }

    /// The next frame is drawn whole (the target's contents or the atlas were
    /// lost).
    pub fn invalidate(&mut self) {
        crate::fast::damage::DamageTracker::invalidate(&mut self.core.fast_frame.damage);
        self.history.clear();
        if let Some((_, source)) = &mut self.flip {
            *source = None;
        }
    }

    /// Draws `scene` into `target`, a view of a `size` texture whose contents
    /// were drawn `age` frames ago (0: unknown), upside down when `flip_y`.
    /// Returns the frame's damage: the rectangles whose pixels differ from the
    /// previous frame's (the whole target for a frame damaged whole), for
    /// `eglSwapBuffersWithDamage` or `eglSetDamageRegion`.
    pub fn draw(
        &mut self,
        scene: &Scene,
        target: &wgpu::TextureView,
        size: Size<DevicePixels>,
        flip_y: bool,
        age: u32,
    ) -> Result<Vec<Bounds<DevicePixels>>> {
        anyhow::ensure!(!self.context.device_lost(), "the GPU device was lost");
        let damage = crate::fast::damage::DamageTracker::frame(
            &mut self.core.fast_frame.damage,
            scene,
            size,
        );
        let rects = damage.rects(size);
        self.history.push_front(match damage {
            Damage::Full => None,
            Damage::Rects(rects) => Some(rects),
        });
        self.history.truncate(HISTORY);

        if !flip_y {
            self.flip = None;
            let region = redraw_region(&self.history, age, size);
            self.draw_region(scene, target, size, region)?;
            return Ok(rects);
        }

        // The texture the scene is drawn into holds the previous frame unless it
        // was just made.
        let device = Arc::clone(&self.core.resources.device);
        let format = self.core.target_format;
        let (blit, source) = self
            .flip
            .get_or_insert_with(|| (FlipBlit::new(&device, format), None));
        let fresh = source.as_ref().is_none_or(|s| s.size != size);
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
        let own = if fresh {
            Region::Full
        } else {
            redraw_region(&self.history, 1, size)
        };
        let view = source.as_ref().map(|s| s.view.clone()).expect("made above");
        self.draw_region(scene, &view, size, own)?;

        let copy = match redraw_region(&self.history, age, size) {
            Region::Nothing if !fresh => return Ok(rects),
            Region::Partial(rect) if !fresh => flip_rect(rect, size.height.0),
            _ => Bounds {
                origin: point(DevicePixels(0), DevicePixels(0)),
                size,
            },
        };
        let (blit, source) = self.flip.as_ref().expect("made above");
        let source = source.as_ref().expect("made above");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flip_blit"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("flip_blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
                copy.origin.x.0 as u32,
                copy.origin.y.0 as u32,
                copy.size.width.0 as u32,
                copy.size.height.0 as u32,
            );
            pass.set_pipeline(&blit.pipeline);
            pass.set_bind_group(0, &source.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.core
            .resources
            .queue
            .submit(std::iter::once(encoder.finish()));
        Ok(rects)
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
            Region::Partial(rect) => Some(rect),
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
