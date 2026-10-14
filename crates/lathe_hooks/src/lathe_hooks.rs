//! Central Lathe hook registry: the single seam between Zed-owned ("upstream")
//! code and Lathe customizations.
//!
//! Upstream installs the registry once via [`init`] and reads extension points
//! from [`LatheHooks`] at single, stable call sites, instead of hosting Lathe
//! feature logic inline in upstream files. Each Lathe feature registers its own
//! callbacks from its own crate, which keeps the upstream files mergeable
//! against future Zed updates. See `lathe-extraction-plan.md` for the seam
//! patterns this enables and the migration order.
//!
//! # Adding an extension point
//!
//! 1. Add a field to [`LatheHooks`] holding your callback, e.g.
//!    `pub editor_dirty_filename: Option<Box<dyn Fn(&Foo, &App) -> Bar>>`.
//!    Prefer `gpui`/primitive types in hook signatures over feature-crate types
//!    so this crate does not take on the feature crates as dependencies (that
//!    would risk dependency cycles).
//! 2. Register it from your feature crate's `init` via [`LatheHooks::update`].
//! 3. Read it from the one upstream call site via [`LatheHooks::get`].

use gpui::{Action, App, Global, SharedString};

#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownFileTarget {
    pub worktree_id: u64,
    pub path: String,
    /// Single-file worktrees have an empty relative path.
    pub file_name: String,
    pub item_id: Option<u64>,
}

/// Registry of Lathe extension points, installed as a GPUI global by [`init`].
///
/// See the module documentation for how to add a new extension point.
#[derive(Default)]
pub struct LatheHooks {
    pub markdown_file_actions:
        Option<fn(MarkdownFileTarget) -> Vec<(SharedString, Box<dyn Action>)>>,
    pub markdown_preview_default: Option<fn(&std::path::Path, &App) -> Option<bool>>,
}

#[derive(Default)]
struct GlobalLatheHooks(LatheHooks);

impl Global for GlobalLatheHooks {}

impl LatheHooks {
    /// Read the installed registry, if [`init`] has run.
    ///
    /// Returns `None` rather than panicking, so upstream read sites stay robust
    /// to init ordering and to entry points that never call [`init`] (such as
    /// some headless or test paths).
    pub fn get(cx: &App) -> Option<&LatheHooks> {
        cx.try_global::<GlobalLatheHooks>().map(|global| &global.0)
    }

    /// Mutate the installed registry, installing a default one first if needed.
    ///
    /// Feature crates call this from their own `init` to register callbacks.
    /// Safe to call before [`init`]: any registrations are preserved when
    /// [`init`] later runs.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut LatheHooks)) {
        f(&mut cx.default_global::<GlobalLatheHooks>().0);
    }
}

/// Install the Lathe hook registry. Call once during application startup.
///
/// Idempotent: a second call leaves any already-registered hooks intact, so it
/// is safe to invoke from each application entry point.
pub fn init(cx: &mut App) {
    if !cx.has_global::<GlobalLatheHooks>() {
        cx.set_global(GlobalLatheHooks::default());
    }
}
