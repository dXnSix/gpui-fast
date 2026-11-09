//! A pipeline cache on disk, so a second start does not rebuild GPUI's pipelines from scratch.
//!
//! wgpu keeps compiled pipelines across starts only when it is given a `wgpu::PipelineCache`
//! (Vulkan today; GL once wgpu's GL backend stores program binaries), and upstream GPUI creates
//! every pipeline with `cache: None`. Here the device asks for `PIPELINE_CACHE` when the adapter
//! offers it, the cache is created from the file named by `wgpu::util::pipeline_cache_key` in
//! `GPUI_PIPELINE_CACHE_DIR` (default: the platform's cache directory) with `fallback: true`,
//! every pipeline is created with it, and its data is written back, atomically, once the
//! pipelines exist. Without the feature or a key nothing is read or written.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Where the cache files go; unset means the platform's cache directory.
pub(crate) const DIR_ENV: &str = "GPUI_PIPELINE_CACHE_DIR";

thread_local! {
    /// The cache of the current device. GPUI creates its device and pipelines on the main
    /// thread and uses one device at a time (a new one after a device loss replaces it), so one
    /// slot on that thread is enough and an old device is never kept alive by it.
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

struct Current {
    device: wgpu::Device,
    cache: wgpu::PipelineCache,
    path: PathBuf,
}

/// The features to request: `PIPELINE_CACHE` when the adapter has it and a cache key exists.
pub(crate) fn features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let offered = adapter.features().contains(wgpu::Features::PIPELINE_CACHE);
    let keyed = wgpu::util::pipeline_cache_key(&adapter.get_info()).is_some();
    if offered && keyed {
        wgpu::Features::PIPELINE_CACHE
    } else {
        wgpu::Features::empty()
    }
}

/// Creates the device's cache from the file on disk, when the device has the feature. A previous
/// device's cache is dropped first, whether or not this one gets a cache.
pub(crate) fn open(adapter: &wgpu::Adapter, device: &wgpu::Device) {
    CURRENT.with(|current| current.borrow_mut().take());
    let Some(path) = cache_file(
        device.features().contains(wgpu::Features::PIPELINE_CACHE),
        wgpu::util::pipeline_cache_key(&adapter.get_info()),
        cache_dir(std::env::var_os(DIR_ENV), platform_cache_dir()),
    ) else {
        return;
    };
    let data = read_data(&path);
    // SAFETY: the data was written by `store` from `get_data()` of a cache of an adapter with the
    // same `pipeline_cache_key` (vendor, device and driver), which is what wgpu requires;
    // `fallback: true` makes wgpu ignore data it cannot use instead of failing.
    let cache = unsafe { device.create_pipeline_cache(&descriptor(data.as_deref())) };
    log::info!(
        "pipeline cache {} ({})",
        path.display(),
        if data.is_some() { "loaded" } else { "new" }
    );
    CURRENT.with(|current| {
        *current.borrow_mut() = Some(Current {
            device: device.clone(),
            cache,
            path,
        })
    });
}

/// The cache to pass to the pipelines `device` creates.
pub(crate) fn get(device: &wgpu::Device) -> Option<wgpu::PipelineCache> {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .filter(|current| current.device == *device)
            .map(|current| current.cache.clone())
    })
}

/// Writes the cache's data back once the pipelines exist.
pub(crate) fn store(device: &wgpu::Device) {
    CURRENT.with(|current| {
        let current = current.borrow();
        let Some(current) = current.as_ref().filter(|current| current.device == *device) else {
            return;
        };
        if let Some(data) = current.cache.get_data()
            && let Err(err) = write_atomic(&current.path, &data)
        {
            log::warn!(
                "writing the pipeline cache {}: {err}",
                current.path.display()
            );
        }
    });
}

pub(crate) fn descriptor(data: Option<&[u8]>) -> wgpu::PipelineCacheDescriptor<'_> {
    wgpu::PipelineCacheDescriptor {
        label: Some("gpui_pipeline_cache"),
        data,
        fallback: true,
    }
}

/// The cache file, or `None` when nothing may be read or written (no feature, no key, no dir).
pub(crate) fn cache_file(
    feature: bool,
    key: Option<String>,
    dir: Option<PathBuf>,
) -> Option<PathBuf> {
    if !feature {
        return None;
    }
    Some(dir?.join(key?))
}

/// `GPUI_PIPELINE_CACHE_DIR` when set and non-empty, else the platform's directory.
pub(crate) fn cache_dir(env: Option<OsString>, platform: Option<PathBuf>) -> Option<PathBuf> {
    match env {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => platform,
    }
}

fn platform_cache_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Caches"))
    } else if cfg!(target_os = "windows") {
        var("LOCALAPPDATA")
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|home| home.join(".cache")))
    };
    base.map(|base| base.join("gpui-fast/pipeline-cache"))
}

/// The file's contents; `None` when it is missing or empty.
pub(crate) fn read_data(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok().filter(|data| !data.is_empty())
}

/// Writes `data` to a temporary file next to `path` and renames it into place, so a reader never
/// sees a partial file.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    let temporary = dir.join(name);
    std::fs::write(&temporary, data)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::fast::pipeline_cache::{cache_dir, cache_file, descriptor, read_data, write_atomic};

    /// A fresh directory under the system's temporary directory.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gpui-pipeline-cache-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const KEY: &str = "wgpu_pipeline_cache_vulkan_4318_8710";

    #[test]
    fn a_second_start_finds_and_uses_the_file() {
        let dir = temp_dir("second-start");
        let path = cache_file(true, Some(KEY.into()), Some(dir.clone())).unwrap();
        assert_eq!(path, dir.join(KEY));
        // First start: nothing on disk, the cache starts empty.
        assert_eq!(read_data(&path), None);
        assert_eq!(descriptor(read_data(&path).as_deref()).data, None);
        // The pipelines exist: their data is stored.
        write_atomic(&path, b"compiled pipelines").unwrap();
        // Second start: the same key finds the file and the cache is created from it.
        let path = cache_file(true, Some(KEY.into()), Some(dir.clone())).unwrap();
        let data = read_data(&path);
        assert_eq!(data.as_deref(), Some(&b"compiled pipelines"[..]));
        assert_eq!(
            descriptor(data.as_deref()).data,
            Some(&b"compiled pipelines"[..])
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_data_falls_back_without_error() {
        let dir = temp_dir("corrupt");
        let path = dir.join(KEY);
        std::fs::write(&path, [0xde, 0xad, 0xbe, 0xef]).unwrap();
        let data = read_data(&path);
        let descriptor = descriptor(data.as_deref());
        assert!(
            descriptor.fallback,
            "wgpu ignores data it cannot use instead of failing"
        );
        assert_eq!(descriptor.data, Some(&[0xde, 0xad, 0xbe, 0xef][..]));
        // An empty (truncated) file counts as no data.
        std::fs::write(&path, []).unwrap();
        assert_eq!(read_data(&path), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_read_or_written_without_the_feature_or_a_key() {
        let dir = temp_dir("no-feature");
        assert_eq!(cache_file(false, Some(KEY.into()), Some(dir.clone())), None);
        assert_eq!(
            cache_file(true, None, Some(dir.clone())),
            None,
            "GL has no key today"
        );
        assert_eq!(cache_file(true, Some(KEY.into()), None), None);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing was written"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_write_is_atomic() {
        let dir = temp_dir("atomic");
        let path = dir.join("nested/dir").join(KEY);
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            [std::ffi::OsString::from(KEY)],
            "no temporary file is left behind"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_directory_comes_from_the_environment_or_the_platform() {
        let platform = Some(PathBuf::from("/home/me/.cache/gpui-fast/pipeline-cache"));
        assert_eq!(
            cache_dir(Some("/opt/app/cache".into()), platform.clone()),
            Some(PathBuf::from("/opt/app/cache"))
        );
        assert_eq!(cache_dir(Some("".into()), platform.clone()), platform);
        assert_eq!(cache_dir(None, platform.clone()), platform);
        assert_eq!(cache_dir(None, None), None);
    }
}
