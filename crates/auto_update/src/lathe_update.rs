use crate::ReleaseAsset;
use anyhow::{Context as _, Result};
use http_client::{HttpClient, HttpClientWithUrl};
use release_channel::ReleaseChannel;
use serde::Deserialize;
use smol::io::AsyncReadExt;
use std::{env, sync::Arc};

/// GitHub API URL for Lathe releases, resolved per channel.
///
/// - `LATHE_BETA_UPDATE_URL` overrides the URL for Beta builds.
/// - `LATHE_UPDATE_URL` overrides the URL for Stable/Preview/Beta builds.
///
/// Both are read from `option_env!` (so CI can bake a different URL into the
/// binary at build time) with a runtime `env::var` fallback for development.
/// When neither is set, Stable/Preview/Beta default to the Lathe fork's
/// GitHub releases endpoint so locally built binaries still get update
/// checks without any environment configuration. Nightly uses Zed Cloud; Dev does not poll.
const LATHE_DEFAULT_UPDATE_URL: &str =
    "https://api.github.com/repos/mina-account/lathe/releases?per_page=30";

pub const PACKAGE_MANAGER_INSTALL_TITLE: &str = "Lathe was installed via a package manager.";
pub const UP_TO_DATE_TITLE: &str = "Lathe is up to date";
pub const UP_TO_DATE_MESSAGE: &str = "You're already running the latest version.";

fn override_value(compiled: Option<&str>, runtime: Option<&str>) -> Option<String> {
    compiled
        .or(runtime)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn resolve_update_url(
    channel: ReleaseChannel,
    beta: Option<String>,
    generic: Option<String>,
) -> Option<String> {
    match channel {
        ReleaseChannel::Beta => Some(
            beta.or(generic)
                .unwrap_or_else(|| LATHE_DEFAULT_UPDATE_URL.to_owned()),
        ),
        ReleaseChannel::Stable | ReleaseChannel::Preview => {
            Some(generic.unwrap_or_else(|| LATHE_DEFAULT_UPDATE_URL.to_owned()))
        }
        ReleaseChannel::Nightly | ReleaseChannel::Dev => None,
    }
}

pub fn update_base_url(channel: ReleaseChannel) -> Option<String> {
    resolve_update_url(
        channel,
        override_value(
            option_env!("LATHE_BETA_UPDATE_URL"),
            env::var("LATHE_BETA_UPDATE_URL").ok().as_deref(),
        ),
        override_value(
            option_env!("LATHE_UPDATE_URL"),
            env::var("LATHE_UPDATE_URL").ok().as_deref(),
        ),
    )
}

fn resolve_notes_url(explicit: Option<String>, api: Option<String>) -> Option<String> {
    explicit.or_else(|| {
        let api = api?;
        api.strip_prefix("https://api.github.com/repos/")
            .and_then(|rest| rest.split_once("/releases"))
            .map(|(owner_repo, _)| format!("https://github.com/{owner_repo}/releases"))
            .or(Some(api))
    })
}

pub fn release_notes_url(channel: ReleaseChannel) -> Option<String> {
    resolve_notes_url(
        override_value(
            option_env!("LATHE_RELEASE_NOTES_URL"),
            env::var("LATHE_RELEASE_NOTES_URL").ok().as_deref(),
        ),
        update_base_url(channel),
    )
}

pub(crate) fn ensure_desktop_artifact(
    asset: &str,
    version: Option<&semver::Version>,
) -> Result<()> {
    anyhow::ensure!(
        asset == "zed",
        "unsupported Lathe fork artifact {asset} (requested version {version:?}); version-pinned remote-server releases are unavailable"
    );
    Ok(())
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize, Clone)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// The tag suffix used by the release workflow for each non-stable channel.
/// Stable has no suffix. Dev has no published releases.
fn channel_tag_suffix(channel: ReleaseChannel) -> Option<&'static str> {
    match channel {
        ReleaseChannel::Stable => Some(""),
        ReleaseChannel::Preview => Some("-preview"),
        ReleaseChannel::Beta => Some("-beta"),
        ReleaseChannel::Nightly => Some("-nightly"),
        ReleaseChannel::Dev => None,
    }
}

fn release_matches_channel(release: &GitHubRelease, channel: ReleaseChannel) -> bool {
    if release.draft {
        return false;
    }
    let Some(suffix) = channel_tag_suffix(channel) else {
        return false;
    };
    if suffix.is_empty() {
        // Stable: tag has no `-foo` suffix at all (e.g. `v0.236.0`).
        !release.tag_name.contains('-')
    } else {
        release.tag_name.ends_with(suffix)
    }
}

/// The release notes for a single published Lathe release.
pub struct ReleaseNotes {
    /// The release name, or its tag when the release has no name.
    pub title: String,
    /// The release body, as Markdown.
    pub notes: String,
}

/// Fetches the release notes for `version` on `channel` from the GitHub
/// releases API. Falls back to the newest release matching the channel when
/// no release is tagged for the running version, which happens for locally
/// built binaries whose version was never published.
pub async fn get_release_notes(
    http_client: Arc<HttpClientWithUrl>,
    api_url: &str,
    channel: ReleaseChannel,
    version: &str,
) -> Result<ReleaseNotes> {
    let releases = get_releases(http_client, api_url).await?;
    let suffix = channel_tag_suffix(channel).unwrap_or_default();
    let version = version.trim_start_matches('v');
    let tag = format!("v{version}{suffix}");

    let release = releases
        .iter()
        .find(|release| !release.draft && release.tag_name == tag)
        .or_else(|| {
            releases
                .iter()
                .find(|release| release_matches_channel(release, channel))
        })
        .with_context(|| format!("no release notes found for {tag} at {api_url}"))?;

    let notes = release
        .body
        .clone()
        .filter(|body| !body.trim().is_empty())
        .with_context(|| format!("release {} has no release notes", release.tag_name))?;

    Ok(ReleaseNotes {
        title: release
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| release.tag_name.clone()),
        notes,
    })
}

async fn get_releases(
    http_client: Arc<HttpClientWithUrl>,
    api_url: &str,
) -> Result<Vec<GitHubRelease>> {
    let mut response = http_client.get(api_url, Default::default(), true).await?;
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;

    anyhow::ensure!(
        response.status().is_success(),
        "failed to fetch release from {api_url}: {:?}",
        String::from_utf8_lossy(&body),
    );

    // Accept either response shape so old binaries with `/releases/latest`
    // baked in keep working, while newer binaries can be pointed at the
    // `/releases` list endpoint to discover prerelease channels (beta,
    // preview) that `/latest` excludes. List responses are returned newest
    // first by the GitHub API, so the first match wins.
    if let Ok(single) = serde_json::from_slice::<GitHubRelease>(&body) {
        Ok(vec![single])
    } else {
        serde_json::from_slice(&body).with_context(|| {
            format!(
                "error deserializing release(s) {:?}",
                String::from_utf8_lossy(&body),
            )
        })
    }
}

pub async fn get_release_asset(
    http_client: Arc<HttpClientWithUrl>,
    api_url: &str,
    channel: ReleaseChannel,
    os: &str,
    arch: &str,
) -> Result<ReleaseAsset> {
    let releases = get_releases(http_client, api_url).await?;
    let release = releases
        .into_iter()
        .find(|release| release_matches_channel(release, channel))
        .with_context(|| format!("no release matched channel {:?} at {api_url}", channel))?;

    let asset = select_asset(&release, os, arch)?;

    // tag_name like "v0.234.7" -> semver "0.234.7".
    let version = release.tag_name.trim_start_matches('v').to_string();

    Ok(ReleaseAsset {
        version,
        url: asset.browser_download_url.clone(),
    })
}

fn select_asset<'a>(release: &'a GitHubRelease, os: &str, arch: &str) -> Result<&'a GitHubAsset> {
    let aliases: &[&str] = match arch {
        "x86_64" => &["x86_64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        other => anyhow::bail!("unsupported architecture: {other}"),
    };
    let version = release
        .tag_name
        .trim_start_matches('v')
        .split('-')
        .next()
        .context("missing release version")?;
    let endings: &[&str] = match os {
        "windows" => &["-windows-setup.exe", "-setup.exe"],
        "macos" => &["-macos.dmg", "-macos.zip"],
        "linux" => &["-linux.tar.gz"],
        other => anyhow::bail!("unsupported os: {other}"),
    };
    for ending in endings {
        for architecture in aliases {
            let canonical = format!("Lathe-{version}-{architecture}{ending}");
            let candidates: Vec<_> = release
                .assets
                .iter()
                .filter(|asset| asset.name == canonical)
                .collect();
            anyhow::ensure!(
                candidates.len() <= 1,
                "ambiguous compatible assets in {}",
                release.tag_name
            );
            if let Some(asset) = candidates.first() {
                return Ok(asset);
            }
        }
    }
    let candidates: Vec<_> = release
        .assets
        .iter()
        .filter(|asset| {
            asset.name.starts_with("Lathe-")
                && (os != "windows"
                    || (!asset.name.contains("-linux-") && !asset.name.contains("-macos-")))
                && asset.name.split('-').any(|part| part == version)
                && aliases.iter().any(|architecture| {
                    endings
                        .iter()
                        .any(|ending| asset.name.ends_with(&format!("-{architecture}{ending}")))
                })
                && (os == "windows" || asset.name.contains(&format!("-{os}.")))
        })
        .collect();
    anyhow::ensure!(
        candidates.len() <= 1,
        "ambiguous compatible assets for os={os} arch={arch} in {}",
        release.tag_name
    );
    candidates.first().copied().with_context(|| {
        format!(
            "no compatible setup asset for os={os} arch={arch} in {}",
            release.tag_name
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, names: &[&str]) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            name: None,
            body: Some("notes".into()),
            draft: false,
            assets: names
                .iter()
                .map(|name| GitHubAsset {
                    name: (*name).into(),
                    browser_download_url: format!(
                        "https://github.com/mina-account/lathe/releases/download/{tag}/{name}"
                    ),
                })
                .collect(),
        }
    }

    #[test]
    fn defaults_and_overrides() {
        for channel in [
            ReleaseChannel::Stable,
            ReleaseChannel::Beta,
            ReleaseChannel::Preview,
        ] {
            let api = resolve_update_url(channel, None, None);
            assert_eq!(api.as_deref(), Some(LATHE_DEFAULT_UPDATE_URL));
            assert_eq!(
                resolve_notes_url(None, api).as_deref(),
                Some("https://github.com/mina-account/lathe/releases")
            );
        }
        for channel in [ReleaseChannel::Dev, ReleaseChannel::Nightly] {
            assert_eq!(
                resolve_update_url(channel, Some("beta".into()), Some("generic".into())),
                None
            );
        }
        assert_eq!(
            resolve_update_url(
                ReleaseChannel::Beta,
                Some("beta".into()),
                Some("generic".into())
            )
            .as_deref(),
            Some("beta")
        );
        assert_eq!(
            resolve_update_url(
                ReleaseChannel::Preview,
                Some("beta".into()),
                Some("generic".into())
            )
            .as_deref(),
            Some("generic")
        );
        assert_eq!(
            override_value(Some("compiled"), Some("runtime")).as_deref(),
            Some("compiled")
        );
        assert_eq!(
            override_value(None, Some("runtime")).as_deref(),
            Some("runtime")
        );
        assert_eq!(override_value(Some(""), Some("runtime")), None);
        assert_eq!(override_value(None, Some("")), None);
        assert_eq!(
            resolve_notes_url(Some("explicit".into()), Some("feed".into())).as_deref(),
            Some("explicit")
        );
    }

    #[test]
    fn windows_assets_and_channel_contract() -> Result<()> {
        let old = release(
            "v1.10.1-beta",
            &[
                "Lathe-Beta-1.10.1-super-compact-c0aca56f4e-x64-setup.exe",
                "Lathe-Beta-1.10.1-super-compact-c0aca56f4e-x64-setup.exe.sha256",
                "Lathe-1.10.1-x86_64-windows.zip",
            ],
        );
        assert!(
            select_asset(&old, "windows", "x86_64")?
                .name
                .contains("super-compact")
        );
        assert!(select_asset(&old, "windows", "aarch64").is_err());
        assert!(select_asset(&old, "linux", "x86_64").is_err());
        assert!(select_asset(&old, "macos", "x86_64").is_err());
        let future = release(
            "v1.10.2-beta",
            &[
                "Lathe-Custom-1.10.2-x64-setup.exe",
                "Lathe-1.10.2-x86_64-windows-setup.exe",
                "Lathe-1.10.2-aarch64-windows-setup.exe",
                "unrelated-x86_64.exe",
            ],
        );
        assert_eq!(
            select_asset(&future, "windows", "x86_64")?.name,
            "Lathe-1.10.2-x86_64-windows-setup.exe"
        );
        assert_eq!(
            select_asset(&future, "windows", "aarch64")?.name,
            "Lathe-1.10.2-aarch64-windows-setup.exe"
        );
        let ambiguous = release(
            "v1.10.2-beta",
            &[
                "Lathe-One-1.10.2-x64-setup.exe",
                "Lathe-Two-1.10.2-x64-setup.exe",
            ],
        );
        assert!(select_asset(&ambiguous, "windows", "x86_64").is_err());
        assert!(release_matches_channel(&future, ReleaseChannel::Beta));
        assert!(!release_matches_channel(&future, ReleaseChannel::Stable));
        assert!(!release_matches_channel(&future, ReleaseChannel::Preview));
        let mut draft = future;
        draft.draft = true;
        assert!(!release_matches_channel(&draft, ReleaseChannel::Beta));
        Ok(())
    }

    #[test]
    fn api_errors_and_notes_remain_on_selected_feed() -> Result<()> {
        use http_client::{FakeHttpClient, Response};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for status in [403, 429, 500] {
            let requests = Arc::new(AtomicUsize::new(0));
            let client = FakeHttpClient::create({
                let requests = requests.clone();
                move |request| {
                    assert_eq!(request.uri().to_string(), LATHE_DEFAULT_UPDATE_URL);
                    requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Ok(Response::builder()
                            .status(status)
                            .body("API failure".into())?)
                    }
                }
            });
            assert!(
                smol::block_on(get_release_asset(
                    client,
                    LATHE_DEFAULT_UPDATE_URL,
                    ReleaseChannel::Beta,
                    "windows",
                    "x86_64"
                ))
                .is_err()
            );
            assert_eq!(requests.load(Ordering::SeqCst), 1);
        }
        let client = FakeHttpClient::create(|request| async move {
            assert_eq!(request.uri().to_string(), LATHE_DEFAULT_UPDATE_URL);
            Ok(Response::builder().status(200).body(r#"[{"tag_name":"v1.10.3","body":"stable"},{"tag_name":"v1.10.2-beta","body":"beta"},{"tag_name":"v1.10.4-beta","body":"draft","draft":true}]"#.into())?)
        });
        let notes = smol::block_on(get_release_notes(
            client.clone(),
            LATHE_DEFAULT_UPDATE_URL,
            ReleaseChannel::Beta,
            "1.10.2",
        ))?;
        assert_eq!(notes.notes, "beta");
        assert!(
            smol::block_on(get_release_asset(
                client,
                LATHE_DEFAULT_UPDATE_URL,
                ReleaseChannel::Beta,
                "windows",
                "x86_64"
            ))
            .is_err()
        );
        let empty = FakeHttpClient::create(|_| async {
            Ok(Response::builder().status(200).body("[]".into())?)
        });
        assert!(
            smol::block_on(get_release_asset(
                empty,
                LATHE_DEFAULT_UPDATE_URL,
                ReleaseChannel::Beta,
                "windows",
                "x86_64"
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn remote_server_requested_version_is_not_discarded() -> Result<()> {
        let requested = semver::Version::parse("1.10.1")?;
        let catalog = release("v1.10.2-beta", &["Lathe-1.10.2-x86_64-windows-setup.exe"]);
        assert!(ensure_desktop_artifact("zed-remote-server", Some(&requested)).is_err());
        assert!(select_asset(&catalog, "windows", "x86_64").is_ok());
        Ok(())
    }
}
