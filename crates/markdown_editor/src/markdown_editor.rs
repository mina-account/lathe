pub mod protocol;

#[cfg(any(
    test,
    all(feature = "browser", any(target_os = "windows", target_os = "linux"))
))]
mod browser_frame;

#[cfg(feature = "application")]
mod item;
#[cfg(feature = "application")]
pub use item::{BrowserSurface, FakeBrowserSurface, MarkdownEditor, init};

#[cfg(feature = "application")]
mod assets;
#[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
pub mod browser;
#[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
mod installation_check;
#[cfg(feature = "application")]
mod opening;
#[cfg(feature = "browser")]
mod packaged_resources;
#[cfg(feature = "application")]
mod settings;
#[cfg(feature = "application")]
pub use opening::{
    CopyHtml, CopyMarkdown, InsertImage, OpenToTheSide, ResetPreferences, SetDefaultOpenMode,
    SetMode, ShowPreview, ToggleOutline,
};
