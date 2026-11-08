//! GL on request: which wgpu backends GPUI's native instance asks for.
//!
//! On Linux, GPUI asks for Vulkan and GL and ranks Vulkan adapters first, so a desktop or CI
//! machine with Vulkan (Mesa's lavapipe included) never exercises the GL ES path TVs take.
//! `WGPU_BACKEND` (wgpu's own reader, e.g. `WGPU_BACKEND=gl`) selects the backends instead.

/// The backends upstream asks for.
const DEFAULT_BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN.union(wgpu::Backends::GL);

/// `wgpu::Backends::from_env()` when `WGPU_BACKEND` is set, else upstream's `VULKAN | GL`.
pub(crate) fn backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(DEFAULT_BACKENDS)
}

#[cfg(test)]
mod tests {
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
