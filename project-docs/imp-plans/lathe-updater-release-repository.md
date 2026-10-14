# Implementation plan: Update Lathe from mina-account releases

Status: Implemented and unit verified. Live installation/update/chat acceptance is deferred to the user's manual testing at their explicit request.
Created: 2026-10-13.
Repository baseline: `c0aca56f4efb908e84c74299595a62fd9b2f0b82`.
Distribution repository: `mina-account/lathe`.

## 1. Objective

Make bundled Lathe builds discover application updates and release notes from `mina-account/lathe`, and complete Windows updates using Lathe's own installation layout. Installing an update must preserve the custom AI chat features, user settings, and user-controlled restart behavior.

The default feed is `https://api.github.com/repos/mina-account/lathe/releases?per_page=30`. The release page is `https://github.com/mina-account/lathe/releases`. Keep the list endpoint because Beta and Preview releases are excluded from GitHub's `/releases/latest` endpoint.

Use `mina-account/lathe` as the distribution repository. Preserve the Git `upstream` remote pointing at `paterschris/lathe` for source synchronization; this work does not change Git remotes or erase upstream attribution.

## 2. Problem and verified constraints

- `crates/auto_update/src/lathe_update.rs:19` defines `LATHE_DEFAULT_UPDATE_URL` using `paterschris/lathe`. `update_base_url` uses a Beta-specific override, a generic override, and that default. Build-time values take precedence over runtime environment values for each override.
- `.github/workflows/release_fork.yml:85`, `:349`, and `:425` bake the old feed into macOS, Linux, and Windows builds. `.github/workflows/lathe_linux_ci.yml:128` also supplies it. Changing only the Rust default leaves these builds on the old repository.
- `release_notes_url` and `get_release_notes` in `lathe_update.rs` derive or fetch release notes through the selected feed. Explicit `LATHE_RELEASE_NOTES_URL` overrides also need coverage.
- `script/release-fork:37` publishes to `paterschris/lathe`. `README.md` download/build instructions and `crates/zed/resources/windows/lathe.iss:73-75` still direct users to that repository. The stable-release Homebrew step in `.github/workflows/release_fork.yml` also clones and pushes `paterschris/homebrew-tap`; it must not run for this fork's releases.
- The local log recorded custom build `1.10.1+beta.c0aca56...` starting at 21:33:52, an upstream update downloading at 21:34:11, and upstream `1.11.2+beta.89.c634ad3...` starting at 21:34:24 on 2026-10-13. The replacement removed `agent.super_compact_mode`. Automatic updates were subsequently disabled in the user's settings, and the custom installer was successfully retested.
- The published fork release `v1.10.1-beta` contains `Lathe-Beta-1.10.1-super-compact-c0aca56f4e-x64-setup.exe` and its checksum. The current asset selector requires the literal Rust architecture string, such as `x86_64`, so this manually named `x64` installer is not discoverable by that selector.
- Windows `InstallerDir::new`, `cleanup_windows`, `install_release_windows`, and `finalize_auto_update_on_quit` in `crates/auto_update/src/auto_update.rs` assume staging and a helper beside a Zed-style editor installation. `install_release_windows` invokes `/verysilent /update=true` and returns `tools/auto_update_helper.exe` as a restart path.
- `script/package-fork-windows.ps1` packages `bin/lathe.exe`, `libexec/lathe-editor.exe`, terminal/GPU runtimes, and icons, but no update helper. `lathe.iss` has no `/update=true` staging protocol or `versions.txt` marker and uses `CloseApplications=force`. Merely selecting this installer does not establish a safe automatic update.
- The existing helper's `JOBS`, file-handle checks, and launch command in `crates/auto_update_helper/src/updater.rs` hard-code `Zed.exe` and the Zed CLI layout. Copying that helper unchanged into the Lathe package is insufficient.
- `AutoUpdater::get_release_asset` is also called for `zed-remote-server`. Its current GitHub branch discards both the requested asset kind and `version: Option<Version>`. A desktop repository change must not silently return a desktop payload or the newest incompatible server version.
- `ReleaseChannel::poll_for_updates` in `crates/release_channel/src/lib.rs:226` excludes only Dev. Although the Lathe resolver returns no GitHub feed for Nightly, bundled Nightly builds can still poll the inherited Zed Cloud path through `AutoUpdater::get_release_asset`. Preserve that distinction and behavior.

## 3. Scope and behavior

### Update destinations and configuration

| Build/channel | Required behavior without explicit overrides |
| --- | --- |
| Stable | Poll published stable releases in `mina-account/lathe`. |
| Beta | Poll published tags ending in `-beta` in `mina-account/lathe`. |
| Preview | Preserve existing `-preview` filtering, using the fork feed. |
| Nightly | Preserve inherited Zed Cloud polling/release behavior; no Lathe GitHub feed. |
| Dev | Preserve no automatic polling and no Lathe GitHub feed. |
| Local bundled and CI release builds | Use the same default repository and release-notes destination. |

Preserve the documented `LATHE_BETA_UPDATE_URL`, `LATHE_UPDATE_URL`, and `LATHE_RELEASE_NOTES_URL` override behavior. Beta-specific overrides continue to win over generic ones; build-time versus runtime precedence remains unchanged. Explicit downstream overrides are allowed. Defaults and this fork's own workflows must not supply the old repository.

No fallback to `paterschris/lathe` is permitted when the fork API is unavailable, has no matching release, or lacks an asset. Stable/Beta/Preview failures must not silently switch to Zed Cloud either. Nightly retains its existing Zed Cloud route and notes links; this is an existing channel route, not fallback for a failed fork request. Retain the existing settings-controlled automatic polling and manual Check for Updates behavior. Propagate actionable fetch/install failures to the existing update UI.

Preserve existing channel filtering and version comparison. Do not install the same version repeatedly or downgrade automatically. Use monotonically increasing fork release versions; build metadata and a changed source SHA alone are not a version bump.

### Release and asset contract

- New releases use the existing `vX.Y.Z` / `vX.Y.Z-beta` / `vX.Y.Z-preview` convention. Exclude drafts and other channels.
- Keep canonical package-script asset names, including `Lathe-<version>-x86_64-windows-setup.exe` and the corresponding `aarch64` name. Verify the running platform and architecture before downloading.
- Accept bounded architecture aliases `x64` for `x86_64` and `arm64` for `aarch64` for existing manual release assets. Cover the already-published custom installer by its actual name; do not rename, delete, or replace that tested public artifact.
- Select a setup executable, not its checksum, a portable ZIP, or an unrelated executable. Prefer canonical names deterministically; fail clearly on incompatible or ambiguous candidates rather than taking an arbitrary first match.
- Missing assets for macOS, Linux, or ARM64 in a Windows-only release must produce a missing-compatible-asset result without substituting Windows x64 or consulting the old fork.
- Keep remote-server requests distinct from desktop updates. For the Stable/Beta/Preview fork-feed branch in this increment, reject remote-server requests with a clear unsupported-artifact error before fetching/selecting a desktop release; the fork currently publishes no compatible version-pinned server assets. Do not silently discard a requested version or return a different/newest server version. Preserve the inherited non-fork Zed Cloud route and its existing version handling. Any later fork server support must match the requested version, channel, platform, and architecture explicitly. Publishing a remote-server matrix is outside this plan.

### Windows installation contract

Reuse the existing updater/helper architecture with an explicit Lathe layout. Preserve the inherited Zed installer/helper path for callers that still use it.

- Resolve one validated Lathe installation root from the running `libexec/lathe-editor.exe`. Use that root consistently for `updates`, `install`, helper discovery, cleanup, rollback, and relaunch. Preserve per-channel directories and stable AppIds.
- Build/package `auto_update_helper` and agree on one location, preferably `<install-root>/tools/auto_update_helper.exe`, so its existing parent-directory model can resolve the installation root. Add the corresponding installer files. Include the helper in signing when signing is configured.
- Add a Lathe `/update=true` mode to `lathe.iss`: stage files under the installation's staging directory, write the agreed completion/version marker only after staging succeeds, and avoid closing or overwriting the running editor during download/staging. Ordinary manual installation remains available.
- Apply the staged update only after the app's normal save/quit or explicit restart path. Cancellation of a quit/save prompt must leave the running version and pending update usable. Background checking must not force-close an editor with unsaved work.
- Extend the helper to move/restore `libexec/lathe-editor.exe`, `bin/lathe.exe`, required runtime files, and affected package resources, then launch the Lathe editor with preserved arguments such as `--user-data-dir`. Keep legacy Zed-layout tests passing.
- Do not overwrite an executing helper. Choose an execution/staging arrangement that permits the helper itself to be upgraded, and prove a second consecutive update using the resulting installed layout.
- Validate staging markers, required payload files, target version/channel, helper availability, and paths before reporting Updated or setting a restart path. Failure retains the current installation or performs a verifiable rollback; errors remain visible.
- Resolve and validate absolute cleanup/move targets within the intended installation/staging roots before recursive filesystem operations. No user configuration, project files, or unrelated channel installations belong in an update transaction.

### Migration and user settings

Old binaries cannot acquire a new compiled feed through a GitHub release description. Ship a newly built bootstrap installer and require one manual upgrade to it. Keep the existing tested `v1.10.1-beta` release available.

Use a new unused application version above the fork's existing 1.10.1 baseline, provisionally `1.10.2` / `v1.10.2-beta`; verify available versions at implementation time. Do not relabel the current installer as a new version or move existing tags. Users who already installed an upstream 1.11.2 build also need the manual bootstrap; do not promise an automatic downgrade.

Do not edit user settings or silently reset `auto_update: false`. Document how users can re-enable automatic updates after installing the corrected build and confirming its version, channel, source commit, and feed. Keep the application-wide default unchanged.

### Out of scope

No application branding/credit rewrite, Git remote changes, Homebrew tap migration, new update service, new dependencies, new channel scheme, telemetry changes, or AI chat behavior changes. Preserve historical provenance links. This planning task does not build, publish, install, or modify application code. The later implementation may publish the new bootstrap release only as an explicit delivery step, without altering the existing tested release.

## 4. Existing code affected

| File | Planned responsibility |
| --- | --- |
| `crates/auto_update/src/lathe_update.rs` | Fork default, override tests, notes URLs, release/channel/asset fixtures and compatible asset selection. |
| `crates/auto_update/src/auto_update.rs` | Preserve asset kind, Windows installation-root/staging/helper integration, status/error/restart validation. |
| `crates/auto_update_helper/src/auto_update_helper.rs` | Lathe layout/root and launch argument handling alongside legacy behavior. |
| `crates/auto_update_helper/src/updater.rs` | Lathe update jobs, launch target, rollback and consecutive-update fixtures. |
| `script/build-fork-windows.ps1` | Build the helper with the same target and release metadata as the editor/CLI. |
| `script/package-fork-windows.ps1` | Stage/sign/package the helper and required update resources. |
| `crates/zed/resources/windows/lathe.iss` | Fork release/support links, helper installation, safe update-mode staging and completion marker. |
| `.github/workflows/release_fork.yml` | Correct all build-time feeds, retain channel-aware publication to the current repository, and prevent this fork from pushing the original Homebrew tap. |
| `.github/workflows/lathe_linux_ci.yml` | Remove the old baked-in feed. |
| `.github/workflows/lathe_windows_ci.yml` | Assert helper/package layout and add isolated update-install smoke coverage. |
| `script/release-fork` | Publish to `mina-account/lathe`; use the existing channel/tag convention consistently. |
| `README.md`, `RELEASE_NOTES.md`, `crates/zed/Cargo.toml`, `Cargo.lock` where applicable | Correct download/source distribution links, bootstrap instructions and release version/evidence. Preserve attribution and historical notes. |

Prefer existing files and a small shared layout helper if needed. Do not add a crate or a generic packaging framework. Review all repository references individually; do not globally replace historical `paterschris` references.

## 5. Implementation tasks

### Task 1: Point application update and release-note discovery at the fork

1. Change the Rust default and all current fork CI build-time update URLs to `mina-account/lathe`. Audit Beta-specific and release-note overrides.
2. Align the release helper's repository and channel-aware tag logic, installer update/support links, and active README download/source instructions. Leave Git remotes, credits and historical release notes intact. Label retained Homebrew instructions as the original distribution, rather than a way to install this fork's additions.
3. Guard the existing Homebrew publication step so it cannot push `paterschris/homebrew-tap` from a `mina-account/lathe` release. Preserve the original workflow's behavior when running in its original repository; migrating or creating a tap is out of scope.
4. Add focused resolver/notes tests covering defaults, precedence, empty values, supported channels and no old-repository fallback. Prefer a pure resolver seam over concurrent mutation of process-wide environment variables.

### Task 2: Establish the fork release and asset compatibility contract

1. Add fixture releases modeled on the actual published Windows-only release plus canonical future assets, mixed channels, drafts, incompatible platforms, missing assets, and ambiguous candidates.
2. Implement deterministic canonical/alias matching and verify release notes use the same selected repository/channel.
3. Add an explicit unsupported-artifact guard for fork remote-server requests at the shared caller. Include a requested-version-A/catalog-version-B regression proving that neither a desktop payload nor version B can be returned. Preserve the existing Zed Cloud remote route/version handling; do not add remote release infrastructure here.
4. Exercise the existing version-comparison path with equal/older releases and the proposed next core version; keep channel and downgrade protections intact.

### Task 3: Make Windows automatic installation match the Lathe package

1. Define and test the shared Lathe root/layout contract before changing file moves.
2. Extend the existing helper, build/package/sign it, and implement safe Inno update-mode staging, completion validation, restart integration, and cleanup/rollback.
3. Cover graceful quit/restart, canceled quit, preserved launch arguments, locked files, missing helper/payload/marker, interrupted staging, failed apply/rollback, and a second consecutive update. Verify ordinary installation and legacy Zed helper behavior still work.
4. Keep this as one coherent integration change: do not enable automatic Windows installation with only half of the installer/helper protocol shipped.

### Task 4: Build and deliver the bootstrap with recorded evidence

1. Recheck repository release versions and choose the next unused core version. Update application version metadata and the matching Beta release tag convention.
2. Run focused Rust tests, changed-file formatting, `git diff --check`, and repository `./script/clippy` (`script/clippy.ps1` on Windows) for changed crates. Use the GPUI test skill/executor timers for GPUI integration fixtures.
3. Parse/lint changed scripts and workflow files. Build a release installer with verified fork defaults/build-time values; record the source commit and packaged CLI version.
4. On an isolated Windows installation/profile, demonstrate old bootstrap/version A -> newer version B from fork release metadata, explicit restart/quit, preserved settings and CLI, version B startup, and a second update. Confirm `agent.super_compact_mode` is accepted and that a real conversation still collapses activity correctly after the bootstrap and after subsequent automatic updates. Use fixtures/local test infrastructure for failure injection; never use the user's working installation for destructive tests.
5. Document manual bootstrap, the opt-in re-enablement of updates, the unsigned/signing state, and release asset/checksum names. Publish a new fork release with verified artifacts when implementation delivery is authorized. Do not mutate `v1.10.1-beta`.

## 6. Acceptance and verification matrix

| Scenario | Evidence required |
| --- | --- |
| No overrides; Stable/Beta/Preview | API requests and HTML/in-app release notes resolve only to `mina-account/lathe`; channel fixture filtering is unchanged. |
| Explicit build/runtime/Beta/notes overrides | Existing precedence is preserved; Dev does not automatically poll, and Nightly retains its inherited route with no Lathe GitHub feed. |
| CI versus local build | Static workflow inspection and built-artifact checks show no old baked-in default feed. |
| API error, rate limit, empty catalog, missing compatible asset | Existing update UI reports failure; no other repository/platform/channel fallback or successful-update claim. |
| Existing `x64` custom release and canonical `x86_64`/`aarch64` setup assets | Correct deterministic installer selection; checksums, ZIPs, unrelated executables and wrong architectures excluded. |
| Equal/older release and next fork release | No repeat or downgrade; a genuinely newer matching release is offered. |
| Remote-server lookup | Fork requests, including version A when only version B exists, return a clear unsupported-artifact error; no desktop or wrong-version payload is returned. Existing Zed Cloud remote routing remains unchanged. |
| Background Windows download/staging with unsaved work | Running editor survives; installed binaries stay unchanged until graceful quit/restart; canceling quit preserves the session. |
| Successful Windows update and next update | Correct Lathe files/runtime/helper layout, channel/AppId, launch arguments, CLI and source version after restart; second update succeeds. |
| Custom AI chat feature after bootstrap and update | `agent.super_compact_mode` remains recognized and enabled; a real conversation shows the expected collapsed activity and settled response after each installed version. |
| Missing helper/marker, partial staging, file lock, failed move | No false Updated state or nonexistent restart target; current installation remains usable or rollback is verified. |
| Manual bootstrap and `auto_update: false` | Correct new build can be installed manually; user preference/settings are preserved and re-enablement is documented. |
| Other supported platform/channel without a release asset | Clear unavailable-compatible-update behavior; no promise of an unbuilt release matrix. |
| Release workflow on this fork | Artifacts publish only to this repository; the original owner's Homebrew tap is never cloned/pushed by the fork release job. |

Live public API reads are useful smoke evidence, but do not replace deterministic fixture tests. Record exact build/profile/commit, installer digest, OS/architecture, source and destination versions, requested feed, and test results. Build the full optimized release for delivery; use focused tests and existing build caches for iteration.

## 7. Completion criteria and rollout risks

- [ ] Default and release-build application feeds and release notes point to `mina-account/lathe`.
- [ ] Active release tooling cannot accidentally publish this fork's artifacts to `paterschris/lathe`.
- [ ] Existing explicit overrides, channels, opt-out settings and version guards remain intact.
- [ ] Fork asset names/platforms/architectures are correctly distinguished; unsupported remote requests cannot receive desktop or wrong-version payloads.
- [ ] The packaged Windows installer, staging protocol, helper and restart path work together without background force-closing user work.
- [ ] A clean bootstrap and two consecutive Windows updates pass with recorded failure/rollback evidence, and Super Compact Mode works afterward.
- [ ] Documentation explains the one-time manual upgrade and optional update re-enablement; existing tested artifacts/tags remain intact.
- [ ] Focused tests, formatting, repository clippy, script/workflow checks and package verification pass.

The release feed is compiled into existing binaries, so publishing a plan or a release alone cannot repair installed copies. A Windows-only release does not provide other platform or remote-server assets. The largest implementation risk is the inherited Zed-versus-Lathe installer/helper mismatch; do not declare completion after a URL-only edit or after a download-only smoke test.

No open product questions remain: the repository is explicit, existing channel/override and user opt-out behavior are preserved, and Windows package compatibility is necessary for the requested updater to work.

## Implementation evidence and manual acceptance

- Runtime source commit: `6ad48ca1e68bf48ec4f466f3cb5e08e283cef81d`; application version `1.10.2`, Beta, Windows x86_64. Built using Rust `1.98.1` and the full optimized release profile with ThinLTO on Windows 11 Pro `10.0.22631`.
- Bootstrap: `Lathe-1.10.2-x86_64-windows-setup.exe`, unsigned. SHA-256: `c3e6a50f3deb02cad9fdf4a1dc5b1c204035600ef3e17766816d1bc170ec3788`. Installer, checksum and archive are retained under `target/updater-artifacts/A/` for delivery and manual testing.
- Build-time update/Beta/notes overrides were cleared. The built editor contains the exact mina-account releases endpoint and does not contain the old baked-in endpoint. Editor, CLI and helper Windows resources identify the same source commit and Beta channel.
- Rust verification: `auto_update` 23 passed, 1 existing ignored network/parking test; `auto_update_helper` 9 passed, including legacy behavior, two fixture transactions, launch arguments, missing payload/marker, wrong target/channel, exclusive file lock, successful rollback, failed launch and backup preservation. Repository clippy, changed-file rustfmt, PowerShell/Bash parsing, workflow YAML parsing and `git diff --check` passed. The ignored integration test is replaced for this task by deterministic API/error/progress fixtures and remains explicitly unverified.
- Baseline branch reconstruction reproduced the old default, native-only alias selector and editor-parent staging failures. Resolver/layout seams and cross-program protocol fixtures were developed together; this was a justified exception to test-first edits. The initial root validator failed its focused test and was corrected before the passing suites.
- Live manual installation, canceled quit, actual version-to-version updates, a second real update and settled Super Compact conversations are **USER MANUAL / NOT VERIFIED**. An attempted local integration smoke did not pass; its original failure was masked by cleanup, so no transaction result is claimed. Cleanup now preserves the first error and waits for exact-root processes/file handles. It was inspected and parsed, without another GUI run.
- At the user's request, GUI/installer tests and additional acceptance builds were stopped; temporary version changes were restored to `1.10.2`. No release was published by the implementation agent, and the existing `v1.10.1-beta` assets/tag were not changed.

Independent product-owner/system-architect review confirmed that the Windows supporting scope is proportionate. Review feedback was incorporated by adding post-update compact-mode acceptance, rejecting version-incompatible fork remote requests explicitly, and correcting the Nightly baseline. A final workflow audit added the original Homebrew tap publication guard without introducing a tap migration.
