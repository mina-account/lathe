use std::{path::Path, sync::OnceLock};

#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
pub use bindings::*;

static API: OnceLock<bindings::Api> = OnceLock::new();

pub fn get_cef_dir() -> Option<std::path::PathBuf> {
    let configured = std::env::var_os("FLATPAK")
        .map(|_| std::path::PathBuf::from("/usr/lib"))
        .or_else(|| std::env::var_os("CEF_PATH").map(std::path::PathBuf::from));
    let platform = format!("cef_{}_{}", std::env::consts::OS, std::env::consts::ARCH);
    let candidates = match configured {
        Some(path) => vec![path.join("154.0.34").join(&platform), path],
        None => vec![std::path::PathBuf::from(env!("OUT_DIR")).join(platform)],
    };
    candidates
        .into_iter()
        .find_map(|path| path.canonicalize().ok())
}

unsafe fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    #[cfg(target_os = "windows")]
    {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
        };
        Ok(unsafe {
            Library::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )?
        }
        .into())
    }
    #[cfg(target_os = "linux")]
    {
        use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
        // Resolve native relocations now so missing dependency symbols reach the UI as load errors.
        Ok(unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL)? }.into())
    }
}

/// Load the complete pinned C API before constructing any CEF-owned values.
///
/// # Safety
/// The caller must validate the trusted runtime and its ABI before use. The
/// library is retained for process lifetime because CEF callbacks can outlive a view.
pub unsafe fn load_library(path: &Path) -> Result<(), String> {
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if let Some(api) = API.get() {
        return if api.path == path {
            Ok(())
        } else {
            Err(format!("CEF is already loaded from {}", api.path.display()))
        };
    }
    let api = unsafe { bindings::Api::load(&path) }?;
    match API.set(api) {
        Ok(()) => Ok(()),
        Err(api) => {
            if API.get().is_some_and(|loaded| loaded.path == api.path) {
                Ok(())
            } else {
                Err("Another CEF runtime was loaded concurrently".into())
            }
        }
    }
}

fn loaded_api() -> &'static bindings::Api {
    match API.get() {
        Some(api) => api,
        None => {
            // Returning dummy C pointers here would turn a load-order bug into memory corruption.
            eprintln!("CEF entrypoint called before successful runtime validation and loading");
            std::process::abort();
        }
    }
}

#[cfg(target_os = "windows")]
impl Default for HWND {
    fn default() -> Self {
        Self(std::ptr::null_mut())
    }
}

#[cfg(target_os = "windows")]
impl Default for HINSTANCE {
    fn default() -> Self {
        Self(std::ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_library_does_not_publish_an_api() {
        let missing =
            std::env::temp_dir().join(format!("lathe-cef-missing-{}", std::process::id()));
        assert!(!missing.exists());
        assert!(unsafe { load_library(&missing) }.is_err());
        assert!(API.get().is_none());
    }

    #[test]
    #[ignore = "requires LATHE_TEST_CEF_LIBRARY pointing to the pinned native runtime"]
    fn complete_pinned_runtime_resolves_every_typed_entrypoint() {
        let path = std::env::var_os("LATHE_TEST_CEF_LIBRARY").expect("explicit pinned runtime");
        let path = Path::new(&path)
            .canonicalize()
            .expect("native fixture path");
        let api = unsafe { bindings::Api::load(&path) }.expect("complete native API");
        assert!(api.path.is_file());
    }

    #[test]
    #[ignore = "requires LATHE_TEST_OTHER_LIBRARY pointing to an isolated native library without the CEF API"]
    fn missing_symbols_do_not_publish_an_api() {
        let path = std::env::var_os("LATHE_TEST_OTHER_LIBRARY").expect("explicit native fixture");
        let path = Path::new(&path)
            .canonicalize()
            .expect("native fixture path");
        let error = match unsafe { bindings::Api::load(&path) } {
            Ok(_) => panic!("Non-CEF library unexpectedly provided the complete API"),
            Err(error) => error,
        };
        assert!(error.contains("cef_api_hash"), "{error}");
        assert!(API.get().is_none());
    }
}
