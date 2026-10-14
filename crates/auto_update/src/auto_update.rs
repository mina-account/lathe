#[cfg(target_os = "windows")]
#[path = "../../auto_update_helper/src/lathe_layout.rs"]
mod lathe_layout;
mod lathe_update;

use anyhow::{Context as _, Result};
use client::Client;
use db::kvp::KeyValueStore;
#[cfg(target_os = "windows")]
use futures::{FutureExt as _, future::Shared};
use futures_lite::StreamExt;
use gpui::{
    App, AppContext as _, AsyncApp, BackgroundExecutor, Context, Entity, EventEmitter, Global,
    Task, TaskExt, Window, actions,
};
use http_client::{HttpClient, HttpClientWithUrl};
use paths::remote_servers_dir;
use release_channel::{AppCommitSha, ReleaseChannel};
use semver::Version;
use serde::{Deserialize, Serialize};
use settings::{RegisterSetting, Settings, SettingsStore};
use smol::fs::File;
use smol::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use std::mem;
use std::{
    env::{
        self,
        consts::{ARCH, OS},
    },
    ffi::OsStr,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use util::command::new_command;
use workspace::Workspace;

const SHOULD_SHOW_UPDATE_NOTIFICATION_KEY: &str = "auto-updater-should-show-updated-notification";

#[derive(Debug)]
struct MissingDependencyError(String);

impl std::fmt::Display for MissingDependencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MissingDependencyError {}
const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);
const NIGHTLY_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
const REMOTE_SERVER_CACHE_LIMIT: usize = 5;

#[cfg(target_os = "linux")]
fn linux_rsync_install_hint() -> &'static str {
    let os_release = match std::fs::read_to_string("/etc/os-release") {
        Ok(os_release) => os_release,
        Err(_) => return "Please install rsync using your package manager",
    };

    let mut distribution_ids = Vec::new();
    for line in os_release.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("ID=") {
            distribution_ids.push(value.trim_matches('"').to_ascii_lowercase());
        } else if let Some(value) = trimmed.strip_prefix("ID_LIKE=") {
            for id in value.trim_matches('"').split_whitespace() {
                distribution_ids.push(id.to_ascii_lowercase());
            }
        }
    }

    let package_manager_hint = if distribution_ids
        .iter()
        .any(|distribution_id| distribution_id == "arch")
    {
        Some("Install it with: sudo pacman -S rsync")
    } else if distribution_ids
        .iter()
        .any(|distribution_id| distribution_id == "debian" || distribution_id == "ubuntu")
    {
        Some("Install it with: sudo apt install rsync")
    } else if distribution_ids.iter().any(|distribution_id| {
        distribution_id == "fedora"
            || distribution_id == "rhel"
            || distribution_id == "centos"
            || distribution_id == "rocky"
            || distribution_id == "almalinux"
    }) {
        Some("Install it with: sudo dnf install rsync")
    } else if distribution_ids
        .iter()
        .any(|distribution_id| distribution_id == "nixos")
    {
        Some("Install pkgs.rsync from nixpkgs")
    } else {
        None
    };

    package_manager_hint.unwrap_or("Please install rsync using your package manager")
}

actions!(
    auto_update,
    [
        /// Checks for available updates.
        Check,
        /// Dismisses the update error message.
        DismissMessage,
        /// Opens the release notes for the current version in a browser.
        ViewReleaseNotes,
    ]
);

#[derive(Serialize, Debug)]
pub struct AssetQuery<'a> {
    asset: &'a str,
    os: &'a str,
    arch: &'a str,
    metrics_id: Option<&'a str>,
    system_id: Option<&'a str>,
    is_staff: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum AutoUpdateStatus {
    Idle,
    Checking,
    Downloading {
        version: Version,
        /// Download progress as a fraction in the range `0.0..=1.0`, or `None`
        /// when the total download size is not yet known.
        progress: Option<f32>,
    },
    Installing {
        version: Version,
    },
    Updated {
        version: Version,
    },
    Errored {
        error: Arc<anyhow::Error>,
    },
}

impl PartialEq for AutoUpdateStatus {
    // `progress` is deliberately not compared: two `Downloading` statuses for
    // the same version are equal regardless of how far the download is.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AutoUpdateStatus::Idle, AutoUpdateStatus::Idle) => true,
            (AutoUpdateStatus::Checking, AutoUpdateStatus::Checking) => true,
            (
                AutoUpdateStatus::Downloading { version: v1, .. },
                AutoUpdateStatus::Downloading { version: v2, .. },
            ) => v1 == v2,
            (
                AutoUpdateStatus::Installing { version: v1 },
                AutoUpdateStatus::Installing { version: v2 },
            ) => v1 == v2,
            (
                AutoUpdateStatus::Updated { version: v1 },
                AutoUpdateStatus::Updated { version: v2 },
            ) => v1 == v2,
            (AutoUpdateStatus::Errored { error: e1 }, AutoUpdateStatus::Errored { error: e2 }) => {
                e1.to_string() == e2.to_string()
            }
            _ => false,
        }
    }
}

impl AutoUpdateStatus {
    pub fn is_updated(&self) -> bool {
        matches!(self, Self::Updated { .. })
    }
}

pub enum AutoUpdateEvent {
    /// A manual check received a release response and found no newer version.
    UpToDate,
}

pub struct AutoUpdater {
    status: AutoUpdateStatus,
    current_version: Version,
    client: Arc<Client>,
    pending_poll: Option<Task<Option<()>>>,
    #[cfg(target_os = "windows")]
    windows_startup: Shared<Task<Result<(), Arc<anyhow::Error>>>>,
    #[cfg(target_os = "windows")]
    pending_windows_update: Option<PendingWindowsUpdate>,
    quit_subscription: Option<gpui::Subscription>,
    update_check_type: UpdateCheckType,
    _wake_subscription: gpui::Subscription,
    dismissed_status: Option<AutoUpdateStatus>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ReleaseAsset {
    pub version: String,
    pub url: String,
}

struct MacOsUnmounter<'a> {
    mount_path: PathBuf,
    background_executor: &'a BackgroundExecutor,
}

impl MacOsUnmounter<'_> {
    /// Unmounts the disk image and waits for completion. This must happen
    /// before the `InstallerDir` is dropped: deleting the temp dir while the
    /// image is still mounted inside it fails silently and leaks the
    /// directory (and the downloaded DMG) in the system temp dir.
    async fn unmount(mut self) {
        let mount_path = mem::take(&mut self.mount_path);
        unmount_disk_image(&mount_path).await;
    }
}

impl Drop for MacOsUnmounter<'_> {
    fn drop(&mut self) {
        let mount_path = mem::take(&mut self.mount_path);
        // Safety net for early exits and cancellation; the happy path calls
        // `unmount`, which leaves the path empty.
        if mount_path.as_os_str().is_empty() {
            return;
        }
        self.background_executor
            .spawn(async move { unmount_disk_image(&mount_path).await })
            .detach();
    }
}

async fn unmount_disk_image(mount_path: &Path) {
    let unmount_output = new_command("hdiutil")
        .args(["detach", "-force"])
        .arg(mount_path)
        .output()
        .await;
    match unmount_output {
        Ok(output) if output.status.success() => {
            log::info!("Successfully unmounted the disk image");
        }
        Ok(output) => {
            log::error!(
                "Failed to unmount disk image: {:?}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(error) => {
            log::error!("Error while trying to unmount disk image: {:?}", error);
        }
    }
}

#[derive(Clone, Copy, Debug, RegisterSetting)]
struct AutoUpdateSetting(bool);

/// Whether or not to automatically check for updates.
///
/// Default: true
impl Settings for AutoUpdateSetting {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self(content.auto_update.unwrap())
    }
}

#[derive(Default)]
struct GlobalAutoUpdate(Option<Entity<AutoUpdater>>);

impl Global for GlobalAutoUpdate {}

pub fn init(client: Arc<Client>, cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_, action, window, cx| check(action, window, cx));

        workspace.register_action(|_, action, _, cx| {
            view_release_notes(action, cx);
        });
    })
    .detach();

    let version = release_channel::AppVersion::global(cx);
    let auto_updater = cx.new(|cx| {
        let updater = AutoUpdater::new(version, client, cx);

        let poll_for_updates = ReleaseChannel::try_global(cx)
            .map(|channel| channel.poll_for_updates())
            .unwrap_or(false);

        if option_env!("ZED_UPDATE_EXPLANATION").is_none()
            && env::var("ZED_UPDATE_EXPLANATION").is_err()
            && poll_for_updates
        {
            let mut update_subscription = AutoUpdateSetting::get_global(cx)
                .0
                .then(|| updater.start_polling(cx));

            cx.observe_global::<SettingsStore>(move |updater: &mut AutoUpdater, cx| {
                if AutoUpdateSetting::get_global(cx).0 {
                    if update_subscription.is_none() {
                        update_subscription = Some(updater.start_polling(cx))
                    }
                } else {
                    update_subscription.take();
                }
            })
            .detach();
        }

        updater
    });
    cx.set_global(GlobalAutoUpdate(Some(auto_updater)));
}

pub fn check(_: &Check, window: &mut Window, cx: &mut App) {
    if let Some(message) = option_env!("ZED_UPDATE_EXPLANATION")
        .map(ToOwned::to_owned)
        .or_else(|| env::var("ZED_UPDATE_EXPLANATION").ok())
    {
        drop(window.prompt(
            gpui::PromptLevel::Info,
            lathe_update::PACKAGE_MANAGER_INSTALL_TITLE,
            Some(&message),
            &["OK"],
            cx,
        ));
        return;
    }

    if !ReleaseChannel::try_global(cx)
        .map(|channel| channel.poll_for_updates())
        .unwrap_or(false)
    {
        return;
    }

    if let Some(updater) = AutoUpdater::get(cx) {
        let already_polling = updater.read(cx).is_polling();
        updater.update(cx, |updater, cx| updater.poll(UpdateCheckType::Manual, cx));

        // Watch for poll completion and surface "you're already up to date"
        // feedback when a manual check finishes with no update available. The
        // existing UI only renders Downloading / Errored / Updated — `Idle`
        // post-check looks identical to "did nothing" without this.
        //
        // Skip if a poll was already in flight when the user clicked Check —
        // the previously-spawned watcher (if it was a manual one) will fire,
        // and we don't want a stack of duplicate prompts.
        if !already_polling {
            let updater_handle = updater.downgrade();
            let window_handle = window.window_handle();
            cx.spawn(async move |cx| {
                // Cap at 30s (200 × 150ms) so a stuck poll doesn't leave a
                // task spinning forever. Exits early as soon as is_polling
                // flips to false.
                for _ in 0..200 {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    let snapshot = cx.update(|app| {
                        updater_handle
                            .upgrade()
                            .map(|u| (u.read(app).is_polling(), u.read(app).status()))
                    });
                    let Some((polling, status)) = snapshot else {
                        return;
                    };
                    if polling {
                        continue;
                    }
                    // Poll complete. `Idle` here means the check succeeded
                    // and there's no update — show a confirmation prompt.
                    // Other statuses (Downloading, Errored, Updated, etc.)
                    // already drive their own UI elsewhere, so leave them.
                    if matches!(status, AutoUpdateStatus::Idle) {
                        let _ = window_handle.update(cx, |_, window, app| {
                            drop(window.prompt(
                                gpui::PromptLevel::Info,
                                lathe_update::UP_TO_DATE_TITLE,
                                Some(lathe_update::UP_TO_DATE_MESSAGE),
                                &["OK"],
                                app,
                            ));
                        });
                    }
                    return;
                }
            })
            .detach();
        }
    } else {
        drop(window.prompt(
            gpui::PromptLevel::Info,
            "Could not check for updates",
            Some("Auto-updates disabled for non-bundled app."),
            &["OK"],
            cx,
        ));
    }
}

pub fn lathe_update_base_url(channel: ReleaseChannel) -> Option<String> {
    lathe_update::update_base_url(channel)
}

pub use lathe_update::ReleaseNotes;

/// Fetches the Lathe release notes for `version` from the GitHub releases API
/// configured for `channel`. Returns `None` when the channel has no releases
/// endpoint (Nightly and Dev).
pub async fn fetch_release_notes(
    http_client: Arc<HttpClientWithUrl>,
    channel: ReleaseChannel,
    version: String,
) -> Result<ReleaseNotes> {
    let api_url = lathe_update_base_url(channel)
        .context("no release notes endpoint is configured for this channel")?;
    lathe_update::get_release_notes(http_client, &api_url, channel, &version).await
}
pub fn release_notes_url(cx: &mut App) -> Option<String> {
    let release_channel = ReleaseChannel::try_global(cx)?;
    // For Stable/Preview/Beta prefer the Lathe GitHub releases page when a
    // LATHE_UPDATE_URL (or LATHE_BETA_UPDATE_URL) is baked in. If unset for
    // Stable/Preview, fall back to the Zed cloud release-notes path.
    let url = match release_channel {
        ReleaseChannel::Stable | ReleaseChannel::Preview => {
            if let Some(notes) = lathe_update::release_notes_url(release_channel) {
                notes
            } else {
                let auto_updater = AutoUpdater::get(cx)?;
                let auto_updater = auto_updater.read(cx);
                let mut current_version = auto_updater.current_version.clone();
                current_version.pre = semver::Prerelease::EMPTY;
                current_version.build = semver::BuildMetadata::EMPTY;
                let release_channel = release_channel.dev_name();
                let path = format!("/releases/{release_channel}/{current_version}");
                auto_updater.client.http_client().build_url(&path)
            }
        }
        ReleaseChannel::Beta => lathe_update::release_notes_url(release_channel)?,
        ReleaseChannel::Nightly => {
            "https://github.com/zed-industries/zed/commits/nightly/".to_string()
        }
        ReleaseChannel::Dev => "https://github.com/zed-industries/zed/commits/main/".to_string(),
    };
    Some(url)
}

pub fn view_release_notes(_: &ViewReleaseNotes, cx: &mut App) -> Option<()> {
    let url = release_notes_url(cx)?;
    cx.open_url(&url);
    None
}

#[cfg(not(target_os = "windows"))]
const INSTALLER_DIR_PREFIX: &str = "zed-auto-update";

#[cfg(not(target_os = "windows"))]
struct InstallerDir(tempfile::TempDir);

#[cfg(not(target_os = "windows"))]
impl InstallerDir {
    async fn new() -> Result<Self> {
        Ok(Self(
            tempfile::Builder::new()
                .prefix(INSTALLER_DIR_PREFIX)
                .tempdir()?,
        ))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(target_os = "windows")]
struct InstallerDir(PathBuf);

#[cfg(target_os = "windows")]
impl InstallerDir {
    async fn new() -> Result<Self> {
        let (root, lathe) = windows_installation_root()?;
        Self::new_at(&root, lathe).await
    }

    async fn new_at(root: &Path, lathe: bool) -> Result<Self> {
        if lathe {
            let install = lathe_layout::validate_tree(root, Path::new("install"))?;
            anyhow::ensure!(
                !install.join("release.txt").exists(),
                "A Lathe update is already staged; restart before downloading another update"
            );
        }
        let installer_dir = root.join("updates");
        lathe_layout::validate_tree(root, Path::new("updates"))?;
        if installer_dir.exists() {
            smol::fs::remove_dir_all(&installer_dir).await?;
        }
        smol::fs::create_dir(&installer_dir).await?;
        Ok(Self(installer_dir))
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UpdateCheckType {
    Automatic,
    Manual,
}

impl UpdateCheckType {
    pub fn is_manual(self) -> bool {
        self == Self::Manual
    }
}

impl EventEmitter<AutoUpdateEvent> for AutoUpdater {}

impl AutoUpdater {
    pub fn get(cx: &mut App) -> Option<Entity<Self>> {
        cx.default_global::<GlobalAutoUpdate>().0.clone()
    }

    fn new(current_version: Version, client: Arc<Client>, cx: &mut Context<Self>) -> Self {
        // On windows, executable files cannot be overwritten while they are
        // running, so we must wait to overwrite the application until quitting
        // or restarting. When quitting the app, we spawn the auto update helper
        // to finish the auto update process after Zed exits. When restarting
        // the app after an update, we use `set_restart_path` to run the auto
        // update helper instead of the app, so that it can overwrite the app
        // and then spawn the new binary.
        #[cfg(target_os = "windows")]
        let quit_subscription = Some(cx.on_app_quit(|_, _| finalize_auto_update_on_quit()));
        #[cfg(not(target_os = "windows"))]
        let quit_subscription = None;

        cx.on_app_restart(|this, _| {
            this.quit_subscription.take();
        })
        .detach();

        // A download or check that was in flight when the machine went to sleep
        // is almost certainly riding a TCP connection that silently died during
        // suspend, so it would otherwise appear to stall indefinitely.
        let wake_subscription = cx.on_system_wake({
            let this = cx.entity().downgrade();
            move |cx| {
                this.update(cx, |this, cx| this.restart_after_wake(cx)).ok();
            }
        });

        #[cfg(all(target_os = "windows", not(test)))]
        let windows_startup = {
            let channel = ReleaseChannel::try_global(cx)
                .unwrap_or(ReleaseChannel::Stable)
                .dev_name();
            let startup = cx.background_spawn(cleanup_windows(channel));
            Self::start_windows_startup(startup, cx)
        };
        #[cfg(all(target_os = "windows", test))]
        let windows_startup = Task::ready(Ok(())).shared();

        Self {
            status: AutoUpdateStatus::Idle,
            current_version,
            client,
            pending_poll: None,
            #[cfg(target_os = "windows")]
            windows_startup,
            #[cfg(target_os = "windows")]
            pending_windows_update: None,
            quit_subscription,
            update_check_type: UpdateCheckType::Automatic,
            _wake_subscription: wake_subscription,
            dismissed_status: None,
        }
    }

    #[cfg(target_os = "windows")]
    fn start_windows_startup(
        startup: Task<Result<Option<PendingWindowsUpdate>>>,
        cx: &mut Context<Self>,
    ) -> Shared<Task<Result<(), Arc<anyhow::Error>>>> {
        cx.spawn(async move |this, cx| {
            let result = startup.await.map_err(Arc::new);
            this.update(cx, |this, cx| match &result {
                Ok(Some(pending)) => this.restore_windows_update(pending.clone(), cx),
                Ok(None) => {}
                Err(error) => {
                    log::error!("Cannot initialize Windows updates: {error:#}");
                    this.status = AutoUpdateStatus::Errored {
                        error: error.clone(),
                    };
                    cx.notify();
                }
            })
            .map_err(Arc::new)?;
            result.map(|_| ())
        })
        .shared()
    }

    #[cfg(target_os = "windows")]
    fn restore_windows_update(&mut self, pending: PendingWindowsUpdate, cx: &mut Context<Self>) {
        cx.set_restart_path(pending.helper.clone());
        self.status = AutoUpdateStatus::Updated {
            version: pending.version.clone(),
        };
        self.pending_windows_update = Some(pending);
        cx.notify();
    }

    fn restart_after_wake(&mut self, cx: &mut Context<Self>) {
        // Only network phases can be safely restarted. `Installing` is a local
        // operation (mounting a dmg, rsync, etc.) that must not be interrupted.
        if !matches!(
            self.status,
            AutoUpdateStatus::Checking | AutoUpdateStatus::Downloading { .. }
        ) {
            return;
        }

        let check_type = self.update_check_type;
        self.pending_poll.take();
        self.status = AutoUpdateStatus::Idle;
        self.poll(check_type, cx);
    }

    pub fn start_polling(&self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let poll_interval =
            ReleaseChannel::try_global(cx).map_or(POLL_INTERVAL, |channel| match channel {
                ReleaseChannel::Nightly => NIGHTLY_POLL_INTERVAL,
                _ => POLL_INTERVAL,
            });

        cx.spawn(async move |this, cx| {
            #[cfg(all(not(target_os = "windows"), not(test)))]
            cx.background_spawn(cleanup_stale_installer_dirs()).detach();

            loop {
                this.update(cx, |this, cx| this.poll(UpdateCheckType::Automatic, cx))?;
                cx.background_executor().timer(poll_interval).await;
            }
        })
    }

    pub fn update_check_type(&self) -> UpdateCheckType {
        self.update_check_type
    }

    pub fn poll(&mut self, check_type: UpdateCheckType, cx: &mut Context<Self>) {
        if check_type.is_manual() {
            self.dismissed_status = None;
        }
        if self.pending_poll.is_some() {
            if self.update_check_type == UpdateCheckType::Automatic {
                self.update_check_type = check_type;
                cx.notify();
            }
            return;
        }
        self.update_check_type = check_type;

        cx.notify();

        self.pending_poll = Some(cx.spawn(async move |this, cx| {
            let result = Self::update(this.upgrade()?, cx).await;
            this.update(cx, |this, cx| {
                this.pending_poll = None;
                if let Err(error) = result {
                    let is_missing_dependency =
                        error.downcast_ref::<MissingDependencyError>().is_some();
                    #[cfg(target_os = "windows")]
                    let startup_failed = this.windows_startup.peek().is_some_and(Result::is_err);
                    #[cfg(not(target_os = "windows"))]
                    let startup_failed = false;
                    this.status = match check_type {
                        UpdateCheckType::Automatic
                            if is_missing_dependency
                                || startup_failed
                                || lathe_update_base_url(
                                    ReleaseChannel::try_global(cx)
                                        .unwrap_or(ReleaseChannel::Stable),
                                )
                                .is_some() =>
                        {
                            log::warn!("auto-update: {}", error);
                            AutoUpdateStatus::Errored {
                                error: Arc::new(error),
                            }
                        }
                        // Be quiet if the check was automated (e.g. when offline)
                        UpdateCheckType::Automatic => {
                            log::info!("auto-update check failed: error:{:?}", error);
                            AutoUpdateStatus::Idle
                        }
                        UpdateCheckType::Manual => {
                            log::error!("auto-update failed: error:{:?}", error);
                            AutoUpdateStatus::Errored {
                                error: Arc::new(error),
                            }
                        }
                    };

                    cx.notify();
                }
            })
            .ok()
        }));
    }

    pub fn current_version(&self) -> Version {
        self.current_version.clone()
    }

    pub fn status(&self) -> AutoUpdateStatus {
        self.status.clone()
    }

    /// Whether a poll request is in flight. Used by the manual `Check`
    /// action's watcher task to know when the check has resolved so it
    /// can surface "you're already up to date" feedback.
    pub fn is_polling(&self) -> bool {
        self.pending_poll.is_some()
    }

    pub fn dismissed_status(&self) -> Option<AutoUpdateStatus> {
        self.dismissed_status.clone()
    }

    pub fn dismiss_status(&mut self, status: AutoUpdateStatus, cx: &mut Context<Self>) {
        self.dismissed_status = Some(status);
        cx.notify();
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if let AutoUpdateStatus::Idle = self.status {
            return false;
        }
        self.status = AutoUpdateStatus::Idle;
        cx.notify();
        true
    }

    // If you are packaging Zed and need to override the place it downloads SSH remotes from,
    // you can override this function. You should also update get_remote_server_release_url to return
    // Ok(None).
    pub async fn download_remote_server_release(
        release_channel: ReleaseChannel,
        version: Option<Version>,
        os: &str,
        arch: &str,
        set_status: impl Fn(&str, &mut AsyncApp) + Send + 'static,
        cx: &mut AsyncApp,
    ) -> Result<PathBuf> {
        let this = cx.update(|cx| {
            cx.default_global::<GlobalAutoUpdate>()
                .0
                .clone()
                .context("auto-update not initialized")
        })?;

        set_status("Fetching remote server release", cx);
        let release = Self::get_release_asset(
            &this,
            release_channel,
            version,
            "zed-remote-server",
            os,
            arch,
            cx,
        )
        .await?;

        let servers_dir = paths::remote_servers_dir();
        let channel_dir = servers_dir.join(release_channel.dev_name());
        let platform_dir = channel_dir.join(format!("{}-{}", os, arch));
        let version_path = platform_dir.join(format!("{}.gz", release.version));
        smol::fs::create_dir_all(&platform_dir).await.ok();

        let client = this.read_with(cx, |this, _| this.client.http_client());

        if smol::fs::metadata(&version_path).await.is_err() {
            log::info!(
                "downloading zed-remote-server {os} {arch} version {}",
                release.version
            );
            set_status("Downloading remote server", cx);
            download_remote_server_binary(&version_path, release, client).await?;
        }

        if let Err(error) =
            cleanup_remote_server_cache(&platform_dir, &version_path, REMOTE_SERVER_CACHE_LIMIT)
                .await
        {
            log::warn!(
                "Failed to clean up remote server cache in {:?}: {error:#}",
                platform_dir
            );
        }

        Ok(version_path)
    }

    pub async fn get_remote_server_release_url(
        channel: ReleaseChannel,
        version: Option<Version>,
        os: &str,
        arch: &str,
        cx: &mut AsyncApp,
    ) -> Result<Option<String>> {
        let this = cx.update(|cx| {
            cx.default_global::<GlobalAutoUpdate>()
                .0
                .clone()
                .context("auto-update not initialized")
        })?;

        let release =
            Self::get_release_asset(&this, channel, version, "zed-remote-server", os, arch, cx)
                .await?;

        Ok(Some(release.url))
    }

    async fn get_release_asset(
        this: &Entity<Self>,
        release_channel: ReleaseChannel,
        version: Option<Version>,
        asset: &str,
        os: &str,
        arch: &str,
        cx: &mut AsyncApp,
    ) -> Result<ReleaseAsset> {
        let client = this.read_with(cx, |this, _| this.client.clone());

        if let Some(api_url) = lathe_update_base_url(release_channel) {
            lathe_update::ensure_desktop_artifact(asset, version.as_ref())?;
            return lathe_update::get_release_asset(
                client.http_client(),
                &api_url,
                release_channel,
                os,
                arch,
            )
            .await;
        }

        let (system_id, metrics_id, is_staff) = if client.telemetry().metrics_enabled() {
            (
                client.telemetry().system_id(),
                client.telemetry().metrics_id(),
                client.telemetry().is_staff(),
            )
        } else {
            (None, None, None)
        };

        let version = if let Some(mut version) = version {
            version.pre = semver::Prerelease::EMPTY;
            version.build = semver::BuildMetadata::EMPTY;
            version.to_string()
        } else {
            "latest".to_string()
        };
        let http_client = client.http_client();

        let path = format!("/releases/{}/{}/asset", release_channel.dev_name(), version,);
        let url = http_client.build_zed_cloud_url_with_query(
            &path,
            AssetQuery {
                os,
                arch,
                asset,
                metrics_id: metrics_id.as_deref(),
                system_id: system_id.as_deref(),
                is_staff,
            },
        )?;

        let mut response = http_client
            .get(url.as_str(), Default::default(), true)
            .await?;
        let mut body = Vec::new();
        response.body_mut().read_to_end(&mut body).await?;

        anyhow::ensure!(
            response.status().is_success(),
            "failed to fetch release: {:?}",
            String::from_utf8_lossy(&body),
        );

        serde_json::from_slice(body.as_slice()).with_context(|| {
            format!(
                "error deserializing release {:?}",
                String::from_utf8_lossy(&body),
            )
        })
    }

    async fn update(this: Entity<Self>, cx: &mut AsyncApp) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            // Retain the shared task on the updater so canceling a check cannot cancel cleanup.
            this.read_with(cx, |this, _| this.windows_startup.clone())
                .await
                .map_err(|error| {
                    anyhow::anyhow!("Windows update initialization failed: {error:#}")
                })?;
            if this.update(cx, |this, cx| {
                if let Some(pending) = this.pending_windows_update.clone() {
                    this.restore_windows_update(pending, cx);
                    true
                } else {
                    this.status.is_updated()
                }
            }) {
                return Ok(());
            }
        }
        let (client, installed_version, previous_status, release_channel) =
            this.read_with(cx, |this, cx| {
                (
                    this.client.http_client(),
                    this.current_version.clone(),
                    this.status.clone(),
                    ReleaseChannel::try_global(cx).unwrap_or(ReleaseChannel::Stable),
                )
            });

        Self::check_dependencies()?;

        this.update(cx, |this, cx| {
            this.status = AutoUpdateStatus::Checking;
            log::info!("Auto Update: checking for updates");
            cx.notify();
        });

        let fetched_release_data =
            Self::get_release_asset(&this, release_channel, None, "zed", OS, ARCH, cx).await?;
        let fetched_version = fetched_release_data.clone().version;
        let app_commit_sha = Ok(cx.update(|cx| AppCommitSha::try_global(cx).map(|sha| sha.full())));
        let newer_version = Self::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version,
            previous_status.clone(),
        )?;

        let Some(newer_version) = newer_version else {
            this.update(cx, |this, cx| {
                this.status = match previous_status {
                    AutoUpdateStatus::Updated { .. } => previous_status,
                    _ => {
                        if this.update_check_type.is_manual() {
                            cx.emit(AutoUpdateEvent::UpToDate);
                        }
                        AutoUpdateStatus::Idle
                    }
                };
                cx.notify();
            });
            return Ok(());
        };

        this.update(cx, |this, cx| {
            this.status = AutoUpdateStatus::Downloading {
                version: newer_version.clone(),
                progress: None,
            };
            cx.notify();
        });

        let installer_dir = InstallerDir::new()
            .await
            .context("Failed to create installer dir")?;
        let target_path = Self::target_path(&installer_dir).await?;
        let progress_entity = this.clone();
        let mut progress_cx = cx.clone();
        download_release(
            &target_path,
            fetched_release_data,
            client,
            move |progress| {
                progress_entity.update(&mut progress_cx, |this, cx| {
                    if let AutoUpdateStatus::Downloading {
                        progress: current_progress,
                        ..
                    } = &mut this.status
                    {
                        *current_progress = progress;
                        cx.notify();
                    }
                });
            },
        )
        .await
        .with_context(|| format!("Failed to download update to {}", target_path.display()))?;

        this.update(cx, |this, cx| {
            this.status = AutoUpdateStatus::Installing {
                version: newer_version.clone(),
            };
            cx.notify();
        });

        #[cfg(test)]
        let install_result = match cx
            .try_read_global::<tests::InstallOverride, _>(|g, _| g.0.clone())
            .map(|test_install| test_install(&target_path, cx))
        {
            Some(result) => result,
            None => return Ok(()),
        };

        #[cfg(not(test))]
        let install_result = {
            let running_app_path = cx.update(|cx| cx.app_path())?;
            let background_executor = cx.background_executor().clone();
            let channel = cx.update(|cx| ReleaseChannel::global(cx).dev_name());
            cx.background_spawn(Self::install_release(
                installer_dir,
                target_path.clone(),
                running_app_path,
                channel,
                background_executor,
                newer_version.to_string(),
            ))
            .await
        };
        let new_binary_path = install_result
            .with_context(|| format!("Failed to install update at: {}", target_path.display()))?;
        #[cfg(target_os = "windows")]
        let pending_windows_update = new_binary_path.as_ref().map(|helper| PendingWindowsUpdate {
            version: newer_version.clone(),
            helper: helper.clone(),
        });
        if let Some(new_binary_path) = new_binary_path {
            cx.update(|cx| cx.set_restart_path(new_binary_path));
        }

        this.update(cx, |this, cx| {
            #[cfg(target_os = "windows")]
            {
                this.pending_windows_update = pending_windows_update;
            }
            this.set_should_show_update_notification(true, cx)
                .detach_and_log_err(cx);
            this.status = AutoUpdateStatus::Updated {
                version: newer_version,
            };
            cx.notify();
        });
        Ok(())
    }

    fn check_if_fetched_version_is_newer(
        release_channel: ReleaseChannel,
        app_commit_sha: Result<Option<String>>,
        installed_version: Version,
        fetched_version: String,
        status: AutoUpdateStatus,
    ) -> Result<Option<Version>> {
        let fetched_version = fetched_version.parse::<Version>()?;

        match release_channel {
            ReleaseChannel::Nightly => {
                let should_download = if let AutoUpdateStatus::Updated { version } = status {
                    fetched_version != version
                } else {
                    let fetched_sha = fetched_version.build.as_str().rsplit('.').next();
                    app_commit_sha
                        .ok()
                        .flatten()
                        .is_none_or(|sha| fetched_sha != Some(sha.as_str()))
                };
                Ok(should_download.then_some(fetched_version))
            }
            // Preserve pre-release tags so "0.234.5-beta.2" > "0.234.5-beta.1" is honored;
            // the non-nightly helper strips the installed pre-release, which would make
            // beta-to-beta bumps compare as not-newer.
            ReleaseChannel::Beta => {
                let current_version = if let AutoUpdateStatus::Updated { version } = status {
                    version
                } else {
                    installed_version
                };
                let should_download = fetched_version > current_version;
                Ok(should_download.then_some(fetched_version))
            }
            _ => {
                let current_version = if let AutoUpdateStatus::Updated { version } = status {
                    version
                } else {
                    installed_version
                };
                Ok(Self::check_if_fetched_version_is_newer_non_nightly(
                    current_version,
                    fetched_version,
                ))
            }
        }
    }

    fn check_dependencies() -> Result<()> {
        #[cfg(target_os = "linux")]
        if which::which("rsync").is_err() {
            let install_hint = linux_rsync_install_hint();
            return Err(MissingDependencyError(format!(
                "rsync is required for auto-updates but is not installed. {install_hint}"
            ))
            .into());
        }

        #[cfg(target_os = "macos")]
        anyhow::ensure!(
            which::which("rsync").is_ok(),
            "Could not auto-update because the required rsync utility was not found."
        );

        Ok(())
    }

    async fn target_path(installer_dir: &InstallerDir) -> Result<PathBuf> {
        let filename = match OS {
            "macos" => anyhow::Ok("Zed.dmg"),
            "linux" => Ok("zed.tar.gz"),
            "windows" => Ok("Zed.exe"),
            unsupported_os => anyhow::bail!("not supported: {unsupported_os}"),
        }?;

        Ok(installer_dir.path().join(filename))
    }

    #[cfg_attr(test, allow(dead_code))]
    async fn install_release(
        installer_dir: InstallerDir,
        target_path: PathBuf,
        running_app_path: PathBuf,
        channel: &str,
        background_executor: BackgroundExecutor,
        _target_version: String,
    ) -> Result<Option<PathBuf>> {
        match OS {
            "macos" => {
                install_release_macos(
                    &installer_dir,
                    &target_path,
                    running_app_path,
                    &background_executor,
                )
                .await
            }
            "linux" => {
                install_release_linux(&installer_dir, &target_path, channel, running_app_path).await
            }
            "windows" => install_release_windows(&target_path, &_target_version, channel).await,
            unsupported_os => anyhow::bail!("not supported: {unsupported_os}"),
        }
    }

    fn check_if_fetched_version_is_newer_non_nightly(
        mut installed_version: Version,
        fetched_version: Version,
    ) -> Option<Version> {
        // For non-nightly releases, ignore build and pre-release fields as they're not provided by our endpoints right now.
        installed_version.pre = semver::Prerelease::EMPTY;
        installed_version.build = semver::BuildMetadata::EMPTY;
        (fetched_version > installed_version).then_some(fetched_version)
    }

    pub fn set_should_show_update_notification(
        &self,
        should_show: bool,
        cx: &App,
    ) -> Task<Result<()>> {
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move {
            if should_show {
                kvp.write_kvp(
                    SHOULD_SHOW_UPDATE_NOTIFICATION_KEY.to_string(),
                    "".to_string(),
                )
                .await?;
            } else {
                kvp.delete_kvp(SHOULD_SHOW_UPDATE_NOTIFICATION_KEY.to_string())
                    .await?;
            }
            Ok(())
        })
    }

    pub fn should_show_update_notification(&self, cx: &App) -> Task<Result<bool>> {
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move {
            Ok(kvp.read_kvp(SHOULD_SHOW_UPDATE_NOTIFICATION_KEY)?.is_some())
        })
    }
}

async fn download_remote_server_binary(
    target_path: &PathBuf,
    release: ReleaseAsset,
    client: Arc<HttpClientWithUrl>,
) -> Result<()> {
    let temp = tempfile::Builder::new().tempfile_in(remote_servers_dir())?;
    let mut temp_file = File::create(&temp).await?;

    let mut response = client.get(&release.url, Default::default(), true).await?;
    anyhow::ensure!(
        response.status().is_success(),
        "failed to download remote server release: {:?}",
        response.status()
    );
    smol::io::copy(response.body_mut(), &mut temp_file).await?;
    smol::fs::rename(&temp, &target_path).await?;

    Ok(())
}

async fn cleanup_remote_server_cache(
    platform_dir: &Path,
    keep_path: &Path,
    limit: usize,
) -> Result<()> {
    if limit == 0 {
        return Ok(());
    }

    let mut entries = smol::fs::read_dir(platform_dir).await?;
    let now = SystemTime::now();
    let mut candidates = Vec::new();

    while let Some(entry) = entries.next().await {
        let entry = entry?;
        let path = entry.path();
        if path.extension() != Some(OsStr::new("gz")) {
            continue;
        }

        let mtime = if path == keep_path {
            now
        } else {
            smol::fs::metadata(&path)
                .await
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        };

        candidates.push((path, mtime));
    }

    if candidates.len() <= limit {
        return Ok(());
    }

    candidates.sort_by(|(path_a, time_a), (path_b, time_b)| {
        time_b.cmp(time_a).then_with(|| path_a.cmp(path_b))
    });

    for (index, (path, _)) in candidates.into_iter().enumerate() {
        if index < limit || path == keep_path {
            continue;
        }

        if let Err(error) = smol::fs::remove_file(&path).await {
            log::warn!(
                "Failed to remove old remote server archive {:?}: {}",
                path,
                error
            );
        }
    }

    Ok(())
}

async fn download_release(
    target_path: &Path,
    release: ReleaseAsset,
    client: Arc<HttpClientWithUrl>,
    mut on_progress: impl FnMut(Option<f32>),
) -> Result<()> {
    let mut target_file = File::create(&target_path).await?;

    let mut response = client.get(&release.url, Default::default(), true).await?;
    anyhow::ensure!(
        response.status().is_success(),
        "failed to download update: {:?}",
        response.status()
    );

    let total_bytes = response
        .headers()
        .get(http_client::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|total_bytes| *total_bytes > 0);

    let mut downloaded_bytes: u64 = 0;
    let mut last_reported_percent: Option<u8> = None;
    let mut buffer = [0u8; 8192];
    let body = response.body_mut();
    loop {
        let bytes_read = body.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        target_file.write_all(&buffer[..bytes_read]).await?;
        downloaded_bytes += bytes_read as u64;

        if let Some(total_bytes) = total_bytes {
            let fraction = (downloaded_bytes as f32 / total_bytes as f32).clamp(0.0, 1.0);
            // Only report when the whole-number percentage changes to avoid notifying the UI on every chunk.
            let percent = (fraction * 100.0) as u8;
            if last_reported_percent != Some(percent) {
                last_reported_percent = Some(percent);
                on_progress(Some(fraction));
            }
        }
    }
    target_file.flush().await?;
    if total_bytes.is_some() && last_reported_percent != Some(100) {
        on_progress(Some(1.0));
    }
    log::info!("downloaded update. path:{:?}", target_path);

    Ok(())
}

async fn install_release_linux(
    temp_dir: &InstallerDir,
    downloaded_tar_gz: &Path,
    channel: &str,
    running_app_path: PathBuf,
) -> Result<Option<PathBuf>> {
    let home_dir = PathBuf::from(env::var("HOME").context("no HOME env var set")?);

    let extracted = temp_dir.path().join("zed");
    fs::create_dir_all(&extracted)
        .await
        .context("failed to create directory into which to extract update")?;

    let mut cmd = new_command("tar");
    cmd.arg("-xzf")
        .arg(&downloaded_tar_gz)
        .arg("-C")
        .arg(&extracted);
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to extract: {cmd}")?;

    anyhow::ensure!(
        output.status.success(),
        "failed to extract {:?} to {:?}: {:?}",
        downloaded_tar_gz,
        extracted,
        String::from_utf8_lossy(&output.stderr)
    );

    let suffix = if channel != "stable" {
        format!("-{}", channel)
    } else {
        String::default()
    };
    let app_folder_name = format!("zed{}.app", suffix);

    let from = extracted.join(&app_folder_name);
    let mut to = home_dir.join(".local");

    let expected_suffix = format!("{}/libexec/zed-editor", app_folder_name);

    if let Some(prefix) = running_app_path
        .to_str()
        .and_then(|str| str.strip_suffix(&expected_suffix))
    {
        to = PathBuf::from(prefix);
    }

    let mut cmd = new_command("rsync");
    cmd.args(["-av", "--delete"]).arg(&from).arg(&to);
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to rsync: {cmd}")?;

    anyhow::ensure!(
        output.status.success(),
        "failed to copy Zed update from {:?} to {:?}: {:?}",
        from,
        to,
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(Some(to.join(expected_suffix)))
}

async fn install_release_macos(
    temp_dir: &InstallerDir,
    downloaded_dmg: &Path,
    running_app_path: PathBuf,
    background_executor: &BackgroundExecutor,
) -> Result<Option<PathBuf>> {
    let running_app_filename = running_app_path
        .file_name()
        .with_context(|| format!("invalid running app path {running_app_path:?}"))?;

    let mountroot = temp_dir.path();
    let mut cmd = new_command("hdiutil");
    cmd.args(["attach", "-nobrowse"])
        .arg(&downloaded_dmg)
        .arg("-mountroot")
        .arg(mountroot);
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to mount: {cmd}")?;

    anyhow::ensure!(
        output.status.success(),
        "failed to mount: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // hdiutil's "attach" output ends with one line per mounted partition:
    //   <dev>\t<scheme>\t<mount-point>
    // The columns are space-padded but tab-delimited, and GPT-formatted DMGs
    // add a verbose Checksumming preamble before the partition rows. Take the
    // last tab-delimited field rather than the last whitespace-delimited
    // token: the volume name is channel-specific and can contain a space
    // ("Lathe Beta"), which whitespace splitting would truncate to "Beta".
    // The mountroot prefix check then rejects preamble and partition-scheme
    // rows, which carry no path under the mountroot we requested.
    // hdiutil reports mount points as fully resolved paths (/private/var/...),
    // while `std::env::temp_dir()` hands back the /var symlink on macOS.
    // `Path::starts_with` matches whole components, so the prefix check never
    // fires unless the mountroot is resolved the same way first.
    let canonical_mountroot = fs::canonicalize(mountroot)
        .await
        .unwrap_or_else(|_| mountroot.to_path_buf());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mount_path = stdout.lines().rev().find_map(|line| {
        let last = line.rsplit('\t').next()?.trim();
        let path = PathBuf::from(last);
        (path.starts_with(&canonical_mountroot) || path.starts_with(mountroot)).then_some(path)
    });

    let Some(mount_path) = mount_path else {
        // The image attached even though we can't tell where. Detach it by
        // device before bailing, otherwise the volume stays mounted for the
        // rest of the login session, the temp dir can never be removed, and
        // its copy of the DMG is stranded on disk.
        if let Some(device) = stdout
            .split_whitespace()
            .find(|token| token.starts_with("/dev/disk"))
        {
            unmount_disk_image(Path::new(device)).await;
        }
        anyhow::bail!("failed to determine DMG mount point from hdiutil output: {stdout}");
    };

    let unmounter = MacOsUnmounter {
        mount_path: mount_path.clone(),
        background_executor,
    };

    let mut mounted_app_path: OsString = mount_path.join(running_app_filename).into();
    mounted_app_path.push("/");

    let mut cmd = new_command("rsync");
    cmd.args(["-av", "--delete", "--exclude", "Icon?"])
        .arg(&mounted_app_path)
        .arg(&running_app_path);
    let rsync_output = cmd.output().await;

    // Await the unmount (even if rsync failed) so that the installer temp dir
    // can be deleted once this function returns.
    unmounter.unmount().await;

    let output = rsync_output.with_context(|| "failed to rsync: {cmd}")?;

    anyhow::ensure!(
        output.status.success(),
        "failed to copy app: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(None)
}

/// Mount points of currently attached volumes that live under `dir`, which
/// must already be canonicalized: `mount` reports fully resolved paths.
#[cfg(any(rust_analyzer, all(not(target_os = "windows"), not(test))))]
async fn mounted_volumes_under(dir: &Path) -> Vec<PathBuf> {
    let Ok(output) = new_command("/sbin/mount").output().await else {
        log::warn!("failed to list mounts while cleaning up installer dirs");
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "<device> on <mount point> (<options>)". Mount points can contain
            // spaces, so split on the surrounding literals rather than columns.
            let (_, rest) = line.split_once(" on ")?;
            let (mount_point, _) = rest.rsplit_once(" (")?;
            let path = PathBuf::from(mount_point);
            path.starts_with(dir).then_some(path)
        })
        .collect()
}

/// Removes stale installer dirs from the system temp dir. Older Zed versions
/// leaked one per update by deleting the dir while the downloaded disk image
/// was still mounted inside it, which made the deletion fail silently.
#[cfg(any(rust_analyzer, all(not(target_os = "windows"), not(test))))]
async fn cleanup_stale_installer_dirs() {
    const STALE_INSTALLER_DIR_AGE: Duration = Duration::from_secs(24 * 60 * 60);

    let temp_dir = std::env::temp_dir();
    let Ok(mut entries) = fs::read_dir(&temp_dir).await else {
        log::warn!("failed to read temp dir {temp_dir:?} while cleaning up installer dirs");
        return;
    };

    let canonical_temp_dir = fs::canonicalize(&temp_dir)
        .await
        .unwrap_or_else(|_| temp_dir.clone());
    let mounted_volumes = mounted_volumes_under(&canonical_temp_dir).await;
    while let Some(entry) = entries.next().await {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(INSTALLER_DIR_PREFIX)
        {
            continue;
        }
        // Leave recent dirs alone, as they may belong to an update currently
        // in progress in another Zed instance.
        let is_stale = entry.metadata().await.ok().is_some_and(|metadata| {
            metadata.is_dir()
                && metadata.modified().ok().is_some_and(|modified| {
                    SystemTime::now()
                        .duration_since(modified)
                        .is_ok_and(|age| age > STALE_INSTALLER_DIR_AGE)
                })
        });
        if is_stale {
            // A disk image leaked by an earlier failed update is still mounted
            // inside the dir, which makes the removal fail with EROFS. Detach
            // it first so the dir (and its copy of the DMG) can actually go.
            let canonical_entry = fs::canonicalize(entry.path())
                .await
                .unwrap_or_else(|_| entry.path());
            for mount_point in mounted_volumes
                .iter()
                .filter(|path| path.starts_with(&canonical_entry))
            {
                unmount_disk_image(mount_point).await;
            }

            if let Err(error) = fs::remove_dir_all(entry.path()).await {
                log::warn!(
                    "failed to remove stale installer dir {:?}: {error}",
                    entry.path()
                );
            } else {
                log::info!("removed stale installer dir {:?}", entry.path());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_installation_root() -> Result<(PathBuf, bool)> {
    let editor = std::env::current_exe()?;
    if let Some(root) = lathe_layout::installation_root(&editor)? {
        Ok((root, true))
    } else {
        Ok((
            editor
                .parent()
                .context("No parent directory for editor")?
                .to_path_buf(),
            false,
        ))
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone)]
struct PendingWindowsUpdate {
    version: Version,
    helper: PathBuf,
}

#[cfg(target_os = "windows")]
#[cfg_attr(test, allow(dead_code))]
async fn cleanup_windows(channel: &str) -> Result<Option<PendingWindowsUpdate>> {
    let (root, lathe) = windows_installation_root()?;
    cleanup_windows_at(&root, lathe, channel).await
}

#[cfg(target_os = "windows")]
async fn cleanup_windows_at(
    root: &Path,
    lathe: bool,
    channel: &str,
) -> Result<Option<PendingWindowsUpdate>> {
    if !lathe {
        if root.join("updates/versions.txt").exists() {
            return Ok(None);
        }
        // Legacy transactions have no validated backup metadata, so retain nonempty directories.
        for directory in ["old", "install", "updates"] {
            let target = lathe_layout::validate_tree(root, Path::new(directory))?;
            match smol::fs::remove_dir(target).await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(None);
    }
    lathe_layout::validate_backup_cleanup(root)?;
    let pending = if root.join("install/release.txt").exists()
        && lathe_layout::validate_payload_version(root, None, Some(channel), ARCH)?.is_gt()
    {
        let marker = std::fs::read_to_string(root.join("install/release.txt"))?;
        let version = Version::parse(marker.lines().nth(1).context("Missing staged version")?)?;
        let helper =
            lathe_layout::validate_tree(root, Path::new("updates/auto_update_helper.exe"))?;
        if !helper.exists() {
            let installed_helper =
                lathe_layout::validate_tree(root, Path::new("tools/auto_update_helper.exe"))?;
            anyhow::ensure!(
                installed_helper.is_file() && std::fs::metadata(&installed_helper)?.len() > 0,
                "Missing Lathe update helper; install the bootstrap manually"
            );
            smol::fs::create_dir_all(root.join("updates")).await?;
            smol::fs::copy(installed_helper, &helper).await?;
        }
        anyhow::ensure!(
            helper.is_file() && std::fs::metadata(&helper)?.len() > 0,
            "Missing or empty staged Lathe update helper"
        );
        Some(PendingWindowsUpdate { version, helper })
    } else {
        None
    };
    let directories: &[&str] = if pending.is_some() {
        &["old"]
    } else {
        &["old", "install", "updates"]
    };
    for directory in directories {
        let target = lathe_layout::validate_tree(root, Path::new(directory))?;
        if target.exists() {
            smol::fs::remove_dir_all(target).await?;
        }
    }
    Ok(pending)
}

async fn install_release_windows(
    downloaded_installer: &Path,
    _target_version: &str,
    _channel: &str,
) -> Result<Option<PathBuf>> {
    let mut command = new_command(downloaded_installer);
    command
        .arg("/verysilent")
        .arg("/update=true")
        .arg("/MERGETASKS=!desktopicon");
    #[cfg(target_os = "windows")]
    let (root, lathe) = windows_installation_root()?;
    #[cfg(target_os = "windows")]
    if lathe {
        lathe_layout::validate_tree(&root, Path::new("install"))?;
        let helper = lathe_layout::validate_tree(&root, Path::new("tools/auto_update_helper.exe"))?;
        anyhow::ensure!(
            helper.is_file(),
            "Missing Lathe update helper; install the bootstrap manually"
        );
        command
            .arg(format!("/DIR={}", root.display()))
            .arg("/NOCLOSEAPPLICATIONS");
    }
    let output = command.output().await?;
    anyhow::ensure!(
        output.status.success(),
        "installer staging failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(target_os = "windows")]
    if lathe {
        let version = semver::Version::parse(_target_version)?;
        let version = format!("{}.{}.{}", version.major, version.minor, version.patch);
        lathe_layout::validate_payload(&root, Some(&version), Some(_channel), ARCH)?;
        let helper =
            lathe_layout::validate_tree(&root, Path::new("updates/auto_update_helper.exe"))?;
        smol::fs::copy(root.join("tools/auto_update_helper.exe"), &helper).await?;
        return Ok(Some(helper));
    }
    let helper_path = std::env::current_exe()?
        .parent()
        .context("No parent dir for editor")?
        .join("tools/auto_update_helper.exe");
    anyhow::ensure!(helper_path.is_file(), "Missing update helper");
    Ok(Some(helper_path))
}

pub async fn finalize_auto_update_on_quit() {
    #[cfg(target_os = "windows")]
    let result: Result<()> = (|| {
        let (root, lathe) = windows_installation_root()?;
        let helper = if lathe {
            if !root.join("install/release.txt").exists() {
                return Ok(());
            }
            lathe_layout::validate_payload(&root, None, None, ARCH)?;
            lathe_layout::validate_tree(&root, Path::new("updates/auto_update_helper.exe"))?
        } else {
            if !root.join("updates/versions.txt").exists() {
                return Ok(());
            }
            root.join("tools/auto_update_helper.exe")
        };
        anyhow::ensure!(helper.is_file(), "Missing staged update helper");
        // The helper must wait for executable handles after the quit callback returns.
        util::command::new_command(helper)
            .args(["--launch", "false"])
            .spawn()?;
        Ok(())
    })();
    #[cfg(target_os = "windows")]
    if let Err(error) = result {
        log::error!("Cannot finalize pending update: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use client::Client;
    use clock::FakeSystemClock;
    use futures::channel::oneshot;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use settings::default_settings;
    use std::{
        rc::Rc,
        sync::{
            Arc,
            atomic::{self, AtomicBool},
        },
    };
    use tempfile::tempdir;

    #[ctor::ctor(unsafe)]
    fn init_logger() {
        zlog::init_test();
    }

    use super::*;

    pub(super) struct InstallOverride(pub Rc<dyn Fn(&Path, &AsyncApp) -> Result<Option<PathBuf>>>);
    impl Global for InstallOverride {}

    #[cfg(target_os = "windows")]
    fn windows_test_updater(
        cx: &mut TestAppContext,
    ) -> (Entity<AutoUpdater>, Arc<std::sync::atomic::AtomicUsize>) {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            move |_| {
                requests.fetch_add(1, atomic::Ordering::SeqCst);
                async move {
                    Ok(Response::builder()
                        .status(429)
                        .body("rate limited".into())?)
                }
            }
        });
        let updater = cx.update(|cx| {
            settings::init(cx);
            release_channel::init_test(Version::new(1, 10, 2), ReleaseChannel::Beta, cx);
            let client = Client::new(Arc::new(FakeSystemClock::new()), http_client, cx);
            cx.new(|cx| AutoUpdater::new(Version::new(1, 10, 2), client, cx))
        });
        (updater, requests)
    }

    #[cfg(target_os = "windows")]
    fn windows_payload(root: &Path, version: &str) -> Result<()> {
        let console = match ARCH {
            "x86_64" => "libexec/x64/OpenConsole.exe",
            "aarch64" => "libexec/arm64/OpenConsole.exe",
            _ => anyhow::bail!("Unsupported test architecture"),
        };
        for relative in [
            "bin/lathe.exe",
            "libexec/lathe-editor.exe",
            "libexec/conpty.dll",
            "share/lathe.ico",
            "tools/auto_update_helper.exe",
            console,
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().context("Missing fixture parent")?)?;
            std::fs::write(path, version)?;
        }
        std::fs::write(
            root.join("release.txt"),
            format!("lathe-update-v1\n{version}\nbeta\n{ARCH}\n"),
        )?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_legacy_preserves_staged_update() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        for directory in ["install/bin", "updates", "old"] {
            std::fs::create_dir_all(root.join(directory))?;
        }
        let files = [
            ("Zed.exe", "running editor"),
            ("install/Zed.exe", "staged editor"),
            ("install/bin/Zed.exe", "staged CLI"),
            ("updates/versions.txt", "1.10.2\n1.10.3\n"),
            ("old/Zed.exe", "rollback editor"),
        ];
        for (relative, contents) in files {
            std::fs::write(root.join(relative), contents)?;
        }

        assert!(smol::block_on(cleanup_windows_at(root, false, "beta"))?.is_none());
        for (relative, contents) in files {
            assert_eq!(std::fs::read_to_string(root.join(relative))?, contents);
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_legacy_preserves_unvalidated_files() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        for directory in ["old", "install", "updates"] {
            std::fs::create_dir(root.join(directory))?;
            std::fs::write(root.join(directory).join("incomplete"), directory)?;
        }

        assert!(smol::block_on(cleanup_windows_at(root, false, "beta"))?.is_none());
        for directory in ["old", "install", "updates"] {
            assert_eq!(
                std::fs::read_to_string(root.join(directory).join("incomplete"))?,
                directory
            );
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_legacy_removes_only_empty_directories() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        for directory in ["old", "install", "updates"] {
            std::fs::create_dir(root.join(directory))?;
        }

        assert!(smol::block_on(cleanup_windows_at(root, false, "beta"))?.is_none());
        for directory in ["old", "install", "updates"] {
            assert!(!root.join(directory).exists());
        }
        assert!(smol::block_on(cleanup_windows_at(root, false, "beta"))?.is_none());
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_preserves_and_repairs_staged_update() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        windows_payload(root, "1.10.2")?;
        windows_payload(&root.join("old"), "1.10.1")?;
        windows_payload(&root.join("install"), "1.10.3")?;
        std::fs::create_dir(root.join("updates"))?;
        let helper = root.join("updates/auto_update_helper.exe");
        std::fs::write(&helper, "staged helper")?;
        let pending = smol::block_on(cleanup_windows_at(root, true, "beta"))?
            .context("Expected pending update")?;
        assert_eq!(pending.version, Version::new(1, 10, 3));
        assert_eq!(pending.helper, helper.canonicalize()?);
        assert_eq!(std::fs::read_to_string(&helper)?, "staged helper");
        assert!(!root.join("old").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("install/bin/lathe.exe"))?,
            "1.10.3"
        );
        assert!(smol::block_on(InstallerDir::new_at(root, true)).is_err());
        assert_eq!(std::fs::read_to_string(&helper)?, "staged helper");
        std::fs::remove_file(&helper)?;
        let repaired = smol::block_on(cleanup_windows_at(root, true, "beta"))?
            .context("Expected repaired pending update")?;
        assert_eq!(std::fs::read_to_string(repaired.helper)?, "1.10.2");
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_discards_staging_after_manual_upgrade() -> Result<()> {
        for installed_version in ["1.10.3", "1.10.4"] {
            let folder = tempdir()?;
            let root = folder.path();
            windows_payload(root, installed_version)?;
            windows_payload(&root.join("install"), "1.10.3")?;
            std::fs::create_dir(root.join("updates"))?;
            std::fs::write(root.join("updates/auto_update_helper.exe"), "staged helper")?;

            assert!(lathe_layout::validate_payload(root, None, Some("beta"), ARCH).is_err());
            assert!(smol::block_on(cleanup_windows_at(root, true, "beta"))?.is_none());
            for directory in ["install", "updates"] {
                assert!(!root.join(directory).exists());
            }
            assert_eq!(
                std::fs::read_to_string(root.join("bin/lathe.exe"))?,
                installed_version
            );
            assert!(smol::block_on(InstallerDir::new_at(root, true)).is_ok());
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_preserves_invalid_superseded_staging() -> Result<()> {
        for (relative, contents) in [
            ("release.txt", "invalid marker".to_owned()),
            ("install/release.txt", "invalid marker".to_owned()),
            ("bin/lathe.exe", String::new()),
            ("install/bin/lathe.exe", String::new()),
            (
                "release.txt",
                format!("lathe-update-v1\n1.10.4\nstable\n{ARCH}\n"),
            ),
            (
                "install/release.txt",
                format!("lathe-update-v1\n1.10.3\nstable\n{ARCH}\n"),
            ),
            (
                "release.txt",
                "lathe-update-v1\n1.10.4\nbeta\nwrong-architecture\n".to_owned(),
            ),
            (
                "install/release.txt",
                "lathe-update-v1\n1.10.3\nbeta\nwrong-architecture\n".to_owned(),
            ),
        ] {
            let folder = tempdir()?;
            let root = folder.path();
            windows_payload(root, "1.10.4")?;
            windows_payload(&root.join("install"), "1.10.3")?;
            std::fs::create_dir(root.join("updates"))?;
            std::fs::write(root.join("updates/auto_update_helper.exe"), "staged helper")?;
            std::fs::write(root.join(relative), contents)?;

            assert!(
                smol::block_on(cleanup_windows_at(root, true, "beta")).is_err(),
                "Invalid {relative} must prevent cleanup"
            );
            assert!(root.join("install/release.txt").is_file());
            assert_eq!(
                std::fs::read_to_string(root.join("updates/auto_update_helper.exe"))?,
                "staged helper"
            );
        }

        let folder = tempdir()?;
        let root = folder.path();
        windows_payload(root, "1.10.4")?;
        windows_payload(&root.join("install"), "1.10.3")?;
        std::fs::create_dir(root.join("old"))?;
        std::fs::write(root.join("old/recovery-file"), "preserve backup")?;
        assert!(smol::block_on(cleanup_windows_at(root, true, "beta")).is_err());
        assert!(root.join("install/release.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(root.join("old/recovery-file"))?,
            "preserve backup"
        );
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_preserves_invalid_transactions() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        windows_payload(root, "1.10.2")?;
        windows_payload(&root.join("install"), "1.10.3")?;
        std::fs::create_dir(root.join("updates"))?;
        let helper = root.join("updates/auto_update_helper.exe");
        std::fs::write(&helper, "preserve helper")?;
        assert!(smol::block_on(cleanup_windows_at(root, true, "stable")).is_err());
        std::fs::remove_file(root.join("install/bin/lathe.exe"))?;
        assert!(smol::block_on(cleanup_windows_at(root, true, "beta")).is_err());
        assert_eq!(std::fs::read_to_string(&helper)?, "preserve helper");
        assert!(root.join("install/release.txt").is_file());
        windows_payload(&root.join("old"), "1.10.2")?;
        assert!(smol::block_on(cleanup_windows_at(root, true, "beta")).is_err());
        assert!(root.join("old/release.txt").is_file());
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cleanup_removes_completed_and_partial_transactions() -> Result<()> {
        let folder = tempdir()?;
        let root = folder.path();
        windows_payload(root, "1.10.2")?;
        windows_payload(&root.join("old"), "1.10.1")?;
        std::fs::create_dir(root.join("install"))?;
        std::fs::write(root.join("install/incomplete"), "partial payload")?;
        std::fs::create_dir(root.join("updates"))?;
        std::fs::write(root.join("updates/download.exe"), "partial download")?;
        assert!(smol::block_on(cleanup_windows_at(root, true, "beta"))?.is_none());
        for directory in ["old", "install", "updates"] {
            assert!(!root.join(directory).exists());
        }
        assert_eq!(
            std::fs::read_to_string(root.join("bin/lathe.exe"))?,
            "1.10.2"
        );
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[gpui::test]
    async fn windows_startup_restores_restart_without_polling(cx: &mut TestAppContext) {
        let folder = tempdir().expect("fixture directory");
        let root = folder.path();
        windows_payload(root, "1.10.2").expect("installed payload");
        windows_payload(&root.join("install"), "1.10.3").expect("staged payload");
        let pending = smol::block_on(cleanup_windows_at(root, true, "beta"))
            .expect("recover pending update")
            .expect("pending update");
        let helper = pending.helper.clone();
        let (updater, requests) = windows_test_updater(cx);
        updater.update(cx, |updater, cx| {
            updater.windows_startup =
                AutoUpdater::start_windows_startup(Task::ready(Ok(Some(pending))), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            updater.read_with(cx, |updater, _| updater.status()),
            AutoUpdateStatus::Updated {
                version: Version::new(1, 10, 3)
            }
        );
        updater.update(cx, |updater, cx| {
            updater.dismiss(cx);
        });
        updater.update(cx, |updater, cx| updater.poll(UpdateCheckType::Manual, cx));
        cx.run_until_parked();
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
        assert_eq!(
            updater.read_with(cx, |updater, _| updater.status()),
            AutoUpdateStatus::Updated {
                version: Version::new(1, 10, 3)
            }
        );
        let restart = cx.expect_restart();
        cx.update(|cx| cx.restart());
        let (restart_path, _) = restart.await.expect("captured test restart");
        assert_eq!(restart_path, Some(helper));
    }

    #[cfg(target_os = "windows")]
    #[gpui::test(iterations = 20)]
    fn windows_startup_serializes_checks_even_after_cancellation(cx: &mut TestAppContext) {
        let (updater, requests) = windows_test_updater(cx);
        let (complete_cleanup, cleanup_completed) = oneshot::channel();
        let startup = cx.background_spawn(async move {
            cleanup_completed.await.context("cleanup completion")?;
            Ok(None)
        });
        updater.update(cx, |updater, cx| {
            updater.windows_startup = AutoUpdater::start_windows_startup(startup, cx);
            updater.poll(UpdateCheckType::Automatic, cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
        updater.update(cx, |updater, cx| {
            updater.poll(UpdateCheckType::Manual, cx);
            updater.pending_poll.take();
            updater.poll(UpdateCheckType::Manual, cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
        complete_cleanup.send(()).expect("finish cleanup");
        cx.run_until_parked();
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 1);
        assert!(!updater.read_with(cx, |updater, _| updater.is_polling()));
        assert!(matches!(
            updater.read_with(cx, |updater, _| updater.status()),
            AutoUpdateStatus::Errored { .. }
        ));
    }

    #[cfg(target_os = "windows")]
    #[gpui::test]
    fn windows_startup_failure_blocks_checks_and_stays_visible(cx: &mut TestAppContext) {
        let (updater, requests) = windows_test_updater(cx);
        cx.update(|cx| {
            release_channel::init_test(Version::new(1, 10, 2), ReleaseChannel::Nightly, cx)
        });
        updater.update(cx, |updater, cx| {
            updater.windows_startup = AutoUpdater::start_windows_startup(
                Task::ready(Err(anyhow::anyhow!(
                    "cleanup failed; preserving rollback backup"
                ))),
                cx,
            );
        });
        cx.run_until_parked();
        for check_type in [UpdateCheckType::Automatic, UpdateCheckType::Manual] {
            updater.update(cx, |updater, cx| updater.poll(check_type, cx));
            cx.run_until_parked();
            assert!(
                matches!(updater.read_with(cx, |updater, _| updater.status()), AutoUpdateStatus::Errored { error } if error.to_string().contains("cleanup failed"))
            );
        }
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
    }

    #[cfg(target_os = "windows")]
    #[gpui::test]
    fn staged_update_blocks_newer_release_checks(cx: &mut TestAppContext) {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            move |_| {
                requests.fetch_add(1, atomic::Ordering::SeqCst);
                async move {
                    Ok(Response::builder()
                        .status(429)
                        .body("unexpected fetch".into())?)
                }
            }
        });
        let updater = cx.update(|cx| {
            settings::init(cx);
            release_channel::init_test(Version::new(1, 10, 2), ReleaseChannel::Beta, cx);
            let client = Client::new(Arc::new(FakeSystemClock::new()), http_client, cx);
            cx.new(|cx| AutoUpdater::new(Version::new(1, 10, 2), client, cx))
        });
        updater.update(cx, |updater, _| {
            updater.status = AutoUpdateStatus::Updated {
                version: Version::new(1, 10, 3),
            };
        });
        for check_type in [UpdateCheckType::Automatic, UpdateCheckType::Manual] {
            updater.update(cx, |updater, cx| updater.poll(check_type, cx));
            cx.run_until_parked();
            assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
            assert_eq!(
                updater.read_with(cx, |updater, _| updater.status()),
                AutoUpdateStatus::Updated {
                    version: Version::new(1, 10, 3)
                }
            );
        }
    }

    #[test]
    fn beta_core_versions_do_not_repeat_or_downgrade() -> Result<()> {
        let current = Version::parse("1.10.2+beta.source")?;
        for (fetched, offered) in [
            ("1.10.1-beta", false),
            ("1.10.2-beta", false),
            ("1.10.3-beta", true),
        ] {
            let result = AutoUpdater::check_if_fetched_version_is_newer(
                ReleaseChannel::Beta,
                Ok(None),
                current.clone(),
                fetched.to_string(),
                AutoUpdateStatus::Idle,
            )?;
            assert_eq!(result.is_some(), offered);
        }
        Ok(())
    }

    #[gpui::test]
    fn fork_api_failure_is_visible_without_fallback(cx: &mut TestAppContext) {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            move |request| {
                requests.fetch_add(1, atomic::Ordering::SeqCst);
                assert_eq!(request.uri().host(), Some("api.github.com"));
                assert_eq!(request.uri().path(), "/repos/mina-account/lathe/releases");
                async move {
                    Ok(Response::builder()
                        .status(429)
                        .body("rate limited".into())?)
                }
            }
        });
        let updater = cx.update(|cx| {
            settings::init(cx);
            release_channel::init_test(Version::new(1, 10, 2), ReleaseChannel::Beta, cx);
            let client = Client::new(Arc::new(FakeSystemClock::new()), http_client, cx);
            cx.new(|cx| AutoUpdater::new(Version::new(1, 10, 2), client, cx))
        });
        updater.update(cx, |updater, cx| {
            updater.poll(UpdateCheckType::Automatic, cx)
        });
        cx.run_until_parked();
        assert!(matches!(
            updater.read_with(cx, |updater, _| updater.status()),
            AutoUpdateStatus::Errored { .. }
        ));
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn fork_remote_server_is_rejected_before_fetch_and_cloud_version_is_preserved(
        cx: &mut TestAppContext,
    ) {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            move |request| {
                requests.fetch_add(1, atomic::Ordering::SeqCst);
                assert_eq!(request.uri().path(), "/releases/nightly/1.10.1/asset");
                assert!(
                    request
                        .uri()
                        .query()
                        .is_some_and(|query| query.contains("asset=zed-remote-server"))
                );
                async move {
                    Ok(Response::builder().status(200).body(
                        r#"{"version":"1.10.1","url":"https://test.example/server-A"}"#.into(),
                    )?)
                }
            }
        });
        let updater = cx.update(|cx| {
            settings::init(cx);
            release_channel::init_test(Version::new(1, 10, 2), ReleaseChannel::Beta, cx);
            let client = Client::new(Arc::new(FakeSystemClock::new()), http_client, cx);
            cx.new(|cx| AutoUpdater::new(Version::new(1, 10, 2), client, cx))
        });
        let mut asynchronous = cx.to_async();
        let rejected = AutoUpdater::get_release_asset(
            &updater,
            ReleaseChannel::Beta,
            Some(Version::new(1, 10, 1)),
            "zed-remote-server",
            "linux",
            "x86_64",
            &mut asynchronous,
        )
        .await;
        assert!(rejected.is_err_and(|error| {
            error
                .to_string()
                .contains("unsupported Lathe fork artifact")
        }));
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 0);
        let server = AutoUpdater::get_release_asset(
            &updater,
            ReleaseChannel::Nightly,
            Some(Version::new(1, 10, 1)),
            "zed-remote-server",
            "linux",
            "x86_64",
            &mut asynchronous,
        )
        .await
        .expect("inherited cloud server route");
        assert_eq!(server.version, "1.10.1");
        assert_eq!(server.url, "https://test.example/server-A");
        assert_eq!(requests.load(atomic::Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn test_auto_update_defaults_to_true(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut store = SettingsStore::new(cx, &settings::default_settings());
            store
                .set_default_settings(&default_settings(), cx)
                .expect("Unable to set default settings");
            store
                .set_user_settings("{}", cx)
                .expect("Unable to set user settings");
            cx.set_global(store);
            assert!(AutoUpdateSetting::get_global(cx).0);
        });
    }

    #[gpui::test]
    #[ignore = "hangs in CI due to allow_parking() + network dependency"]
    async fn test_auto_update_downloads(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        zlog::init_test();
        let release_available = Arc::new(AtomicBool::new(false));

        let (dmg_tx, dmg_rx) = oneshot::channel::<String>();

        cx.update(|cx| {
            settings::init(cx);

            let current_version = semver::Version::new(0, 100, 0);
            release_channel::init_test(current_version, ReleaseChannel::Stable, cx);

            let clock = Arc::new(FakeSystemClock::new());
            let release_available = Arc::clone(&release_available);
            let dmg_rx = Arc::new(parking_lot::Mutex::new(Some(dmg_rx)));
            let fake_client_http = FakeHttpClient::create(move |req| {
                let release_available = release_available.load(atomic::Ordering::Relaxed);
                let dmg_rx = dmg_rx.clone();
                async move {
                if req.uri().path() == "/releases/stable/latest/asset" {
                    if release_available {
                        return Ok(Response::builder().status(200).body(
                            r#"{"version":"0.100.1","url":"https://test.example/new-download"}"#.into()
                        ).unwrap());
                    } else {
                        return Ok(Response::builder().status(200).body(
                            r#"{"version":"0.100.0","url":"https://test.example/old-download"}"#.into()
                        ).unwrap());
                    }
                } else if req.uri().path() == "/new-download" {
                    return Ok(Response::builder().status(200).body({
                        let dmg_rx = dmg_rx.lock().take().unwrap();
                        dmg_rx.await.unwrap().into()
                    }).unwrap());
                }
                Ok(Response::builder().status(404).body("".into()).unwrap())
                }
            });
            let client = Client::new(clock, fake_client_http, cx);
            crate::init(client, cx);
        });

        let auto_updater = cx.update(|cx| AutoUpdater::get(cx).expect("auto updater should exist"));

        cx.background_executor.run_until_parked();

        auto_updater.read_with(cx, |updater, _| {
            assert_eq!(updater.status(), AutoUpdateStatus::Idle);
            assert_eq!(updater.current_version(), semver::Version::new(0, 100, 0));
        });

        release_available.store(true, atomic::Ordering::SeqCst);
        cx.background_executor.advance_clock(POLL_INTERVAL);
        cx.background_executor.run_until_parked();

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            cx.background_executor
                .timer(Duration::from_millis(10))
                .await;
            cx.run_until_parked();
            let status = auto_updater.read_with(cx, |updater, _| updater.status());
            if !matches!(status, AutoUpdateStatus::Idle) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for status to leave Idle (current: {status:?})");
            }
        }
        let status = auto_updater.read_with(cx, |updater, _| updater.status());
        assert_eq!(
            status,
            AutoUpdateStatus::Downloading {
                version: semver::Version::new(0, 100, 1),
                progress: None,
            }
        );

        dmg_tx.send("<fake-zed-update>".to_owned()).unwrap();

        let tmp_dir = Arc::new(tempdir().unwrap());

        cx.update(|cx| {
            let tmp_dir = tmp_dir.clone();
            cx.set_global(InstallOverride(Rc::new(move |target_path, _cx| {
                let tmp_dir = tmp_dir.clone();
                let dest_path = tmp_dir.path().join("zed");
                std::fs::copy(&target_path, &dest_path)?;
                Ok(Some(dest_path))
            })));
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            cx.background_executor
                .timer(Duration::from_millis(10))
                .await;
            cx.run_until_parked();
            let status = auto_updater.read_with(cx, |updater, _| updater.status());
            if !matches!(status, AutoUpdateStatus::Downloading { .. }) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for status to leave Downloading (current: {status:?})");
            }
        }
        let status = auto_updater.read_with(cx, |updater, _| updater.status());
        assert_eq!(
            status,
            AutoUpdateStatus::Updated {
                version: semver::Version::new(0, 100, 1)
            }
        );
        let will_restart = cx.expect_restart();
        cx.update(|cx| cx.restart());
        let (path, arguments) = will_restart.await.unwrap();
        assert!(arguments.is_empty());
        let path = path.unwrap();
        assert_eq!(path, tmp_dir.path().join("zed"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "<fake-zed-update>");
    }

    #[gpui::test]
    async fn test_download_release_reports_progress(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();

        let body = vec![0u8; 20_000];
        let content_length = body.len();

        let client = FakeHttpClient::create(move |_req| {
            let body = body.clone();
            async move {
                Ok(Response::builder()
                    .status(200)
                    .header(
                        http_client::http::header::CONTENT_LENGTH,
                        body.len().to_string(),
                    )
                    .body(body.into())
                    .unwrap())
            }
        });

        let temp_dir = tempdir().unwrap();
        let target_path = temp_dir.path().join("zed-download");
        let release = ReleaseAsset {
            version: "1.0.0".to_string(),
            url: "https://test.example/download".to_string(),
        };

        let reported = Rc::new(std::cell::RefCell::new(Vec::<f32>::new()));
        download_release(&target_path, release, client, {
            let reported = reported.clone();
            move |fraction| {
                if let Some(fraction) = fraction {
                    reported.borrow_mut().push(fraction);
                }
            }
        })
        .await
        .unwrap();

        let reported = reported.borrow();
        assert!(
            reported.len() >= 2,
            "expected progress to be reported across multiple reads, got {reported:?}"
        );
        assert_eq!(
            reported.last().copied(),
            Some(1.0),
            "download should finish at 100%"
        );
        for fraction in reported.iter() {
            assert!(
                (0.0..=1.0).contains(fraction),
                "progress {fraction} out of range"
            );
        }
        for pair in reported.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "progress must not decrease: {reported:?}"
            );
        }

        let downloaded_len = std::fs::metadata(&target_path).unwrap().len();
        assert_eq!(downloaded_len, content_length as u64);
    }

    #[gpui::test]
    async fn test_download_release_without_content_length_reports_no_progress(
        cx: &mut TestAppContext,
    ) {
        cx.background_executor.allow_parking();

        let body = vec![0u8; 20_000];
        let content_length = body.len();

        let client = FakeHttpClient::create(move |_req| {
            let body = body.clone();
            async move { Ok(Response::builder().status(200).body(body.into()).unwrap()) }
        });

        let temp_dir = tempdir().unwrap();
        let target_path = temp_dir.path().join("zed-download");
        let release = ReleaseAsset {
            version: "1.0.0".to_string(),
            url: "https://test.example/download".to_string(),
        };

        let reported = Rc::new(std::cell::RefCell::new(Vec::<Option<f32>>::new()));
        download_release(&target_path, release, client, {
            let reported = reported.clone();
            move |fraction| {
                reported.borrow_mut().push(fraction);
            }
        })
        .await
        .unwrap();

        assert!(
            reported.borrow().is_empty(),
            "progress should not be reported when the total size is unknown, got {:?}",
            reported.borrow()
        );

        let downloaded_len = std::fs::metadata(&target_path).unwrap().len();
        assert_eq!(downloaded_len, content_length as u64);
    }

    #[test]
    fn test_stable_does_not_update_when_fetched_version_is_not_higher() {
        let release_channel = ReleaseChannel::Stable;
        let app_commit_sha = Ok(Some("a".to_string()));
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Idle;
        let fetched_version = semver::Version::new(1, 0, 0);

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.to_string(),
            status,
        );

        assert_eq!(newer_version.unwrap(), None);
    }

    #[test]
    fn test_stable_does_update_when_fetched_version_is_higher() {
        let release_channel = ReleaseChannel::Stable;
        let app_commit_sha = Ok(Some("a".to_string()));
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Idle;
        let fetched_version = semver::Version::new(1, 0, 1);

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.to_string(),
            status,
        );

        assert_eq!(newer_version.unwrap(), Some(fetched_version));
    }

    #[test]
    fn test_stable_does_not_update_when_fetched_version_is_not_higher_than_cached() {
        let release_channel = ReleaseChannel::Stable;
        let app_commit_sha = Ok(Some("a".to_string()));
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Updated {
            version: semver::Version::new(1, 0, 1),
        };
        let fetched_version = semver::Version::new(1, 0, 1);

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.to_string(),
            status,
        );

        assert_eq!(newer_version.unwrap(), None);
    }

    #[test]
    fn test_stable_does_update_when_fetched_version_is_higher_than_cached() {
        let release_channel = ReleaseChannel::Stable;
        let app_commit_sha = Ok(Some("a".to_string()));
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Updated {
            version: semver::Version::new(1, 0, 1),
        };
        let fetched_version = semver::Version::new(1, 0, 2);

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.to_string(),
            status,
        );

        assert_eq!(newer_version.unwrap(), Some(fetched_version));
    }

    #[test]
    fn test_nightly_does_not_update_when_fetched_sha_is_same() {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(Some("a".to_string()));
        let mut installed_version = semver::Version::new(1, 0, 0);
        installed_version.build = semver::BuildMetadata::new("a").unwrap();
        let status = AutoUpdateStatus::Idle;
        let fetched_version = "1.0.0+a".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version,
            status,
        );

        assert_eq!(newer_version.unwrap(), None);
    }

    #[test]
    fn test_nightly_does_update_when_fetched_sha_is_not_same() {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(Some("a".to_string()));
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Idle;
        let fetched_version = "1.0.0+b".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.clone(),
            status,
        );

        assert_eq!(
            newer_version.unwrap(),
            Some(fetched_version.parse().unwrap())
        );
    }

    #[test]
    fn test_nightly_does_not_update_when_fetched_version_is_same_as_cached() {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(Some("a".to_string()));
        let mut installed_version = semver::Version::new(1, 0, 0);
        installed_version.build = semver::BuildMetadata::new("a").unwrap();
        let status = AutoUpdateStatus::Updated {
            version: "1.0.0+b".parse().unwrap(),
        };
        let fetched_version = "1.0.0+b".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version,
            status,
        );

        assert_eq!(newer_version.unwrap(), None);
    }

    #[test]
    fn test_nightly_does_update_when_fetched_sha_is_not_same_as_cached() {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(Some("a".to_string()));
        let mut installed_version = semver::Version::new(1, 0, 0);
        installed_version.build = semver::BuildMetadata::new("a").unwrap();
        let status = AutoUpdateStatus::Updated {
            version: "1.0.0+b".parse().unwrap(),
        };
        let fetched_version = "1.0.0+c".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.clone(),
            status,
        );

        assert_eq!(
            newer_version.unwrap(),
            Some(fetched_version.parse().unwrap())
        );
    }

    #[test]
    fn test_nightly_does_not_redownload_after_updating_to_fetched_version() {
        let release_channel = ReleaseChannel::Nightly;
        let installed_version = semver::Version::new(1, 0, 0);
        let fetched_version = "1.0.0+nightly.b".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            Ok(Some("a".to_string())),
            installed_version.clone(),
            fetched_version.clone(),
            AutoUpdateStatus::Idle,
        )
        .unwrap()
        .expect("a newer nightly version should be available");

        let next_check = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            Ok(Some("a".to_string())),
            installed_version,
            fetched_version,
            AutoUpdateStatus::Updated {
                version: newer_version,
            },
        );

        assert_eq!(next_check.unwrap(), None);
    }

    #[test]
    fn test_nightly_does_update_when_installed_versions_sha_cannot_be_retrieved() {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(None);
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Idle;
        let fetched_version = "1.0.0+a".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.clone(),
            status,
        );

        assert_eq!(
            newer_version.unwrap(),
            Some(fetched_version.parse().unwrap())
        );
    }

    #[test]
    fn test_nightly_does_not_update_when_cached_update_is_same_as_fetched_and_installed_versions_sha_cannot_be_retrieved()
     {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(None);
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Updated {
            version: "1.0.0+b".parse().unwrap(),
        };
        let fetched_version = "1.0.0+b".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version,
            status,
        );

        assert_eq!(newer_version.unwrap(), None);
    }

    #[test]
    fn test_nightly_does_update_when_cached_update_is_not_same_as_fetched_and_installed_versions_sha_cannot_be_retrieved()
     {
        let release_channel = ReleaseChannel::Nightly;
        let app_commit_sha = Ok(None);
        let installed_version = semver::Version::new(1, 0, 0);
        let status = AutoUpdateStatus::Updated {
            version: "1.0.0+b".parse().unwrap(),
        };
        let fetched_version = "1.0.0+c".to_string();

        let newer_version = AutoUpdater::check_if_fetched_version_is_newer(
            release_channel,
            app_commit_sha,
            installed_version,
            fetched_version.clone(),
            status,
        );

        assert_eq!(
            newer_version.unwrap(),
            Some(fetched_version.parse().unwrap())
        );
    }
}
