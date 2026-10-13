pub mod protocol;

#[cfg(feature = "application")]
mod item;
#[cfg(feature = "application")]
pub use item::{BrowserSurface, FakeBrowserSurface, MarkdownEditor, init};

#[cfg(feature = "application")]
mod assets;
#[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
pub mod browser;
#[cfg(feature = "browser")]
mod packaged_resources;
#[cfg(feature = "application")]
mod settings;
