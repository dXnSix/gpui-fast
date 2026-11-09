//! Draws GPUI's holes (`gpui::fast::hole`): polychrome sprites with the
//! reserved texture id [`gpui::AtlasTextureId::hole`] clear the target to
//! transparent black inside their rounded bounds instead of blending.
//!
//! The pipeline shares the polychrome sprite vertex shader and draws in the
//! scene's order like any batch. Its fragment outputs the hole's coverage as
//! alpha and blends with `dst * (1 - src_alpha)` on every channel, so the
//! inside becomes transparent black, antialiased edges and rounded corners
//! fade to the old pixels, and what is drawn later blends over the hole.

use gpui::AtlasTextureId;

/// The hole fragment, appended to the renderer's WGSL. It uses the
/// polychrome sprite's varyings and helpers, and the rounded clip coverage
/// that `fast::rounded_clip` adds to the module.
const SHADER: &str = r#"
@fragment
fn fs_hole(input: PolySpriteVarying) -> @location(0) vec4<f32> {
    if (any(input.clip_distances < vec4<f32>(0.0))) {
        return vec4<f32>(0.0);
    }
    let sprite = load_poly_sprite(input.sprite_id);
    let distance = quad_sdf(input.position.xy, sprite.bounds, sprite.corner_radii);
    return fast_clip_color(vec4<f32>(0.0, 0.0, 0.0, saturate(0.5 - distance)), input.position.xy, true);
}
"#;

/// The renderer's shader module source with the hole fragment.
pub(crate) fn shader_source(source: &str) -> wgpu::ShaderSource<'static> {
    crate::fast::rounded_clip::shader_source(&format!("{source}\n{SHADER}"))
}

/// Whether a polychrome batch of `texture` is a batch of holes.
pub(crate) fn is_hole(texture: AtlasTextureId) -> bool {
    texture.is_hole()
}

/// Keeps the destination scaled by one minus the hole's coverage.
const CLEAR: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::Zero,
    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
    operation: wgpu::BlendOperation::Add,
};

/// The hole pipeline and the texture its vertex shader is bound to.
pub(crate) struct Holes {
    pub(crate) pipeline: wgpu::RenderPipeline,
    /// A 1×1 texture: the polychrome sprite vertex shader reads the sprite
    /// texture's size, which a hole never uses.
    pub(crate) texture: wgpu::BindGroup,
}

/// The hole pipeline for targets of `format`, on the polychrome sprites'
/// layouts.
pub(crate) fn pipeline(
    device: &wgpu::Device,
    layouts: &crate::wgpu_renderer::WgpuBindGroupLayouts,
    module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    cache: Option<&wgpu::PipelineCache>,
) -> Holes {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hole_texture"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("hole_sampler"),
        ..Default::default()
    });
    let texture = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("hole_texture_bind_group"),
        layout: &layouts.texture,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let mut bind_group_layouts = vec![
        Some(&layouts.globals),
        Some(&layouts.instances),
        Some(&layouts.texture),
    ];
    crate::fast::rounded_clip::pipeline_layouts(&mut bind_group_layouts, layouts);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("hole_layout"),
        bind_group_layouts: &bind_group_layouts,
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("hole"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_poly_sprite"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_hole"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState {
                    color: CLEAR,
                    alpha: CLEAR,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache,
    });
    Holes { pipeline, texture }
}

#[cfg(test)]
mod tests;
