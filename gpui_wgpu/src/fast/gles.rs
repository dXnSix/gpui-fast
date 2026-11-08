// Modified by Longbridge for gpui-fast.
//! GL ES devices: when GPUI draws through its instance-texture path, and what it asks a GL
//! adapter for.
//!
//! GL ES 3.0 has no storage buffers, and some GL ES 3.1+ drivers expose none to vertex shaders.
//! The renderer already has a path for that, the WebGL2 one (`shaders_webgl.wgsl`, instance
//! records decoded from a texture); upstream turns it on only when compiled for the web. The
//! decisions here turn it on for every GL adapter that needs it: native GL ES TVs, emulators
//! and desktop GL drivers without vertex storage.
//!
//! On Linux, GPUI asks for Vulkan and GL and ranks Vulkan adapters first, so a desktop or CI
//! machine with Vulkan (Mesa's lavapipe included) never exercises the GL ES path TVs take.
//! `WGPU_BACKEND` (wgpu's own reader, e.g. `WGPU_BACKEND=gl`) selects the backends instead.

/// What the decisions need to know about an adapter, so they can be tested without a GPU.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AdapterFacts {
    pub(crate) backend: wgpu::Backend,
    /// `DownlevelFlags::VERTEX_STORAGE`: vertex shaders may read storage buffers.
    pub(crate) vertex_storage: bool,
    pub(crate) max_storage_buffers_per_shader_stage: u32,
    /// Compiled for the web, where GL is WebGL2.
    pub(crate) wasm: bool,
}

impl AdapterFacts {
    pub(crate) fn of(adapter: &wgpu::Adapter) -> Self {
        Self {
            backend: adapter.get_info().backend,
            vertex_storage: adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::VERTEX_STORAGE),
            max_storage_buffers_per_shader_stage: adapter
                .limits()
                .max_storage_buffers_per_shader_stage,
            wasm: cfg!(target_family = "wasm"),
        }
    }

    fn uses_instance_textures(&self) -> bool {
        self.backend == wgpu::Backend::Gl
            && (self.wasm || !self.vertex_storage || self.max_storage_buffers_per_shader_stage == 0)
    }
}

/// Whether the renderer decodes instance data from a texture (the WebGL2 path) instead of
/// reading storage buffers: on every GL adapter without vertex storage, and on the web as before.
pub(crate) fn uses_instance_textures(adapter: &wgpu::Adapter) -> bool {
    AdapterFacts::of(adapter).uses_instance_textures()
}

/// The device limits: the WebGL2 downlevel limits (within the adapter's) whenever instance
/// textures are used, else `upstream`'s choice. A GL ES 3.0 adapter cannot give
/// `downlevel_defaults()`, so asking for them makes `request_device` fail. Logs the chosen path
/// once per device.
pub(crate) fn required_limits(adapter: &wgpu::Adapter, upstream: wgpu::Limits) -> wgpu::Limits {
    let facts = AdapterFacts::of(adapter);
    let limits = limits_for(&facts, &adapter.limits(), upstream);
    let info = adapter.get_info();
    if facts.uses_instance_textures() {
        log::info!(
            "{} ({:?}): instance textures and WebGL2 limits (no vertex storage buffers)",
            info.name,
            info.backend
        );
    } else {
        log::info!(
            "{} ({:?}): storage-buffer instance data",
            info.name,
            info.backend
        );
    }
    limits
}

pub(crate) fn limits_for(
    facts: &AdapterFacts,
    adapter_limits: &wgpu::Limits,
    upstream: wgpu::Limits,
) -> wgpu::Limits {
    if facts.uses_instance_textures() {
        wgpu::Limits::downlevel_webgl2_defaults()
            .using_resolution(adapter_limits.clone())
            .using_alignment(adapter_limits.clone())
    } else {
        upstream
    }
}

/// The atlas format for GL adapters: `Rgba8Unorm` (GL ES has no BGRA textures without an
/// extension), when the adapter can sample and upload it. `None` leaves the choice to upstream.
pub(crate) fn atlas_format(adapter: &wgpu::Adapter) -> Option<wgpu::TextureFormat> {
    let required = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let rgba = adapter
        .get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm)
        .allowed_usages
        .contains(required);
    atlas_format_for(&AdapterFacts::of(adapter), rgba)
}

pub(crate) fn atlas_format_for(
    facts: &AdapterFacts,
    rgba_usable: bool,
) -> Option<wgpu::TextureFormat> {
    (facts.backend == wgpu::Backend::Gl && rgba_usable).then_some(wgpu::TextureFormat::Rgba8Unorm)
}

/// The backends upstream asks for.
const DEFAULT_BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN.union(wgpu::Backends::GL);

/// `wgpu::Backends::from_env()` when `WGPU_BACKEND` is set, else upstream's `VULKAN | GL`.
pub(crate) fn backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(DEFAULT_BACKENDS)
}

#[cfg(test)]
mod tests {
    mod device {
        use crate::fast::gles::{AdapterFacts, atlas_format_for, limits_for};

        fn gl(vertex_storage: bool, max_storage: u32) -> AdapterFacts {
            AdapterFacts {
                backend: wgpu::Backend::Gl,
                vertex_storage,
                max_storage_buffers_per_shader_stage: max_storage,
                wasm: false,
            }
        }

        /// A GL ES 3.0 adapter: what wgpu reports for Mali GL ES 3.0 drivers and the emulator.
        fn gles30_limits() -> wgpu::Limits {
            wgpu::Limits {
                max_texture_dimension_2d: 8192,
                max_storage_buffers_per_shader_stage: 0,
                ..wgpu::Limits::downlevel_webgl2_defaults()
            }
        }

        #[test]
        fn gles30_uses_instance_textures_and_webgl2_limits() {
            let facts = gl(false, 0);
            assert!(facts.uses_instance_textures());
            let limits = limits_for(&facts, &gles30_limits(), wgpu::Limits::downlevel_defaults());
            assert_eq!(limits.max_storage_buffers_per_shader_stage, 0);
            assert_eq!(
                limits.max_texture_dimension_2d, 8192,
                "within the adapter's resolution"
            );
            assert!(
                limits.check_limits(&gles30_limits()),
                "the adapter can give them"
            );
            assert_eq!(
                atlas_format_for(&facts, true),
                Some(wgpu::TextureFormat::Rgba8Unorm)
            );
        }

        #[test]
        fn gles31_without_vertex_storage_uses_instance_textures() {
            // Storage buffers exist, but not in vertex shaders.
            let facts = gl(false, 4);
            assert!(facts.uses_instance_textures());
            // Or the flag is set but no buffers are exposed.
            assert!(gl(true, 0).uses_instance_textures());
        }

        #[test]
        fn gl_with_vertex_storage_keeps_storage_buffers_but_rgba_atlases() {
            let facts = gl(true, 8);
            assert!(!facts.uses_instance_textures());
            let upstream = wgpu::Limits::downlevel_defaults();
            let limits = limits_for(&facts, &wgpu::Limits::default(), upstream.clone());
            assert_eq!(limits, upstream);
            assert_eq!(
                atlas_format_for(&facts, true),
                Some(wgpu::TextureFormat::Rgba8Unorm)
            );
            assert_eq!(
                atlas_format_for(&facts, false),
                None,
                "unusable RGBA leaves it to upstream"
            );
        }

        #[test]
        fn vulkan_is_unchanged() {
            let facts = AdapterFacts {
                backend: wgpu::Backend::Vulkan,
                vertex_storage: false,
                max_storage_buffers_per_shader_stage: 0,
                wasm: false,
            };
            assert!(!facts.uses_instance_textures());
            let upstream = wgpu::Limits::downlevel_defaults();
            assert_eq!(
                limits_for(&facts, &wgpu::Limits::default(), upstream.clone()),
                upstream
            );
            assert_eq!(atlas_format_for(&facts, true), None);
        }

        #[test]
        fn wasm_gl_keeps_the_webgl2_path() {
            let facts = AdapterFacts {
                wasm: true,
                ..gl(true, 8)
            };
            assert!(facts.uses_instance_textures());
            let limits = limits_for(
                &facts,
                &wgpu::Limits::default(),
                wgpu::Limits::downlevel_defaults(),
            );
            assert_eq!(
                limits.max_storage_buffers_per_shader_stage,
                wgpu::Limits::downlevel_webgl2_defaults().max_storage_buffers_per_shader_stage
            );
        }
    }

    mod backends {
        use std::sync::Mutex;

        use crate::fast::gles::{DEFAULT_BACKENDS, backends};

        /// The tests change the process environment, so they run one at a time.
        static ENV: Mutex<()> = Mutex::new(());

        fn with_backend_env(value: Option<&str>, check: impl FnOnce()) {
            let _guard = ENV
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::var_os("WGPU_BACKEND");
            // SAFETY: the ENV lock serializes every test that touches the environment.
            unsafe {
                match value {
                    Some(value) => std::env::set_var("WGPU_BACKEND", value),
                    None => std::env::remove_var("WGPU_BACKEND"),
                }
            }
            check();
            // SAFETY: as above.
            unsafe {
                match previous {
                    Some(previous) => std::env::set_var("WGPU_BACKEND", previous),
                    None => std::env::remove_var("WGPU_BACKEND"),
                }
            }
        }

        #[test]
        fn unset_keeps_vulkan_and_gl() {
            with_backend_env(None, || assert_eq!(backends(), DEFAULT_BACKENDS));
        }

        #[test]
        fn gl_selects_only_gl() {
            with_backend_env(Some("gl"), || assert_eq!(backends(), wgpu::Backends::GL));
        }

        #[test]
        fn vulkan_selects_only_vulkan() {
            with_backend_env(Some("vulkan"), || {
                assert_eq!(backends(), wgpu::Backends::VULKAN)
            });
        }
    }
}
