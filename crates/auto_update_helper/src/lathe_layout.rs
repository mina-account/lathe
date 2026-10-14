use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

pub(crate) const MARKER: &str = "lathe-update-v1";
pub(crate) const DIRECTORIES: &[&str] = &["bin", "libexec", "share", "tools"];

pub(crate) fn installation_root(editor: &Path) -> Result<Option<PathBuf>> {
    if editor
        .file_name()
        .is_none_or(|name| name != "lathe-editor.exe")
    {
        return Ok(None);
    }
    let editor = editor
        .canonicalize()
        .context("Cannot resolve Lathe editor")?;
    let libexec = editor.parent().context("Missing libexec directory")?;
    anyhow::ensure!(
        libexec.file_name().is_some_and(|name| name == "libexec"),
        "Lathe editor must be installed in libexec"
    );
    let root = libexec
        .parent()
        .context("Missing Lathe installation root")?
        .to_path_buf();
    validate_tree(&root, Path::new("libexec"))?;
    Ok(Some(root))
}

pub(crate) fn validate_tree(root: &Path, relative: &Path) -> Result<PathBuf> {
    anyhow::ensure!(root.is_absolute(), "Installation root must be absolute");
    let root = root
        .canonicalize()
        .context("Cannot resolve installation root")?;
    let mut target = root.clone();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            anyhow::bail!("Unsafe update path: {}", relative.display());
        };
        target.push(component);
        if target.exists() {
            let metadata = std::fs::symlink_metadata(&target)?;
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Update path is a symbolic link: {}",
                target.display()
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                anyhow::ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "Update path is a reparse point: {}",
                    target.display()
                );
            }
            anyhow::ensure!(
                target.canonicalize()?.starts_with(&root),
                "Update path escapes installation root"
            );
        }
    }
    if target.is_dir() {
        for entry in std::fs::read_dir(&target)? {
            let child = entry?.path();
            validate_tree(&root, child.strip_prefix(&root)?)?;
        }
    }
    Ok(target)
}

pub(crate) fn validate_payload(
    root: &Path,
    version: Option<&str>,
    channel: Option<&str>,
    architecture: &str,
) -> Result<()> {
    let install = validate_tree(root, Path::new("install"))?;
    for directory in DIRECTORIES {
        anyhow::ensure!(
            install.join(directory).is_dir(),
            "Incomplete Lathe update: missing {directory} directory"
        );
    }
    let marker = std::fs::read_to_string(install.join("release.txt"))
        .context("Incomplete Lathe update: missing completion marker")?;
    let fields: Vec<_> = marker.lines().collect();
    anyhow::ensure!(
        fields.len() == 4 && fields.first().copied() == Some(MARKER),
        "Invalid Lathe update marker"
    );
    let target_version = fields.get(1).context("Missing target version")?;
    let target_channel = fields.get(2).context("Missing target channel")?;
    anyhow::ensure!(
        version.is_none_or(|expected| *target_version == expected),
        "Lathe update version does not match requested release"
    );
    anyhow::ensure!(
        channel.is_none_or(|expected| *target_channel == expected),
        "Lathe update channel does not match running channel"
    );
    anyhow::ensure!(
        fields.get(3).copied() == Some(architecture),
        "Lathe update architecture does not match running architecture"
    );
    let installed = std::fs::read_to_string(root.join("release.txt"))
        .context("Missing installed Lathe release metadata; install the bootstrap manually")?;
    let installed: Vec<_> = installed.lines().collect();
    anyhow::ensure!(
        installed.first().copied() == Some(MARKER)
            && installed.get(2) == fields.get(2)
            && installed.get(3) == fields.get(3),
        "Lathe update crosses installation channel or architecture"
    );
    anyhow::ensure!(
        numeric_version(target_version)?
            > numeric_version(installed.get(1).context("Missing installed version")?)?,
        "Lathe update must be newer than installed version"
    );
    let console = match architecture {
        "x86_64" => "libexec/x64/OpenConsole.exe",
        "aarch64" => "libexec/arm64/OpenConsole.exe",
        _ => anyhow::bail!("Unsupported Lathe architecture"),
    };
    for required in [
        "bin/lathe.exe",
        "libexec/lathe-editor.exe",
        "libexec/conpty.dll",
        "share/lathe.ico",
        "tools/auto_update_helper.exe",
        console,
    ] {
        anyhow::ensure!(
            install.join(required).is_file()
                && std::fs::metadata(install.join(required))?.len() > 0,
            "Incomplete Lathe update: missing {required}"
        );
    }
    Ok(())
}

pub(crate) fn validate_backup_cleanup(root: &Path) -> Result<()> {
    if !root.join("old").exists() {
        return Ok(());
    }
    let current = std::fs::read_to_string(root.join("release.txt"))
        .context("Missing installed release metadata; preserving rollback backup")?;
    let old = std::fs::read_to_string(root.join("old/release.txt"))
        .context("Interrupted Lathe update; preserving rollback backup for manual recovery")?;
    let current: Vec<_> = current.lines().collect();
    let old: Vec<_> = old.lines().collect();
    anyhow::ensure!(
        current.len() == 4
            && old.len() == 4
            && current.first().copied() == Some(MARKER)
            && old.first().copied() == Some(MARKER)
            && current.get(2) == old.get(2)
            && current.get(3) == old.get(3),
        "Invalid rollback metadata; preserving backup"
    );
    anyhow::ensure!(
        numeric_version(current.get(1).context("Missing current version")?)?
            > numeric_version(old.get(1).context("Missing old version")?)?,
        "Lathe update has not completed; preserving backup"
    );
    for required in [
        "bin/lathe.exe",
        "libexec/lathe-editor.exe",
        "libexec/conpty.dll",
        "tools/auto_update_helper.exe",
        "share/lathe.ico",
    ] {
        anyhow::ensure!(
            root.join(required).is_file(),
            "Current Lathe installation is incomplete; preserving backup"
        );
    }
    Ok(())
}

fn numeric_version(value: &str) -> Result<Vec<u64>> {
    let parts: Vec<_> = value
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<_, _>>()?;
    anyhow::ensure!(parts.len() == 3, "Invalid Lathe core version");
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_and_paths() -> Result<()> {
        let folder = tempfile::tempdir()?;
        std::fs::create_dir(folder.path().join("libexec"))?;
        let editor = folder.path().join("libexec/lathe-editor.exe");
        std::fs::write(&editor, "editor")?;
        assert_eq!(
            installation_root(&editor)?,
            Some(folder.path().canonicalize()?)
        );
        assert!(validate_tree(folder.path(), Path::new("../outside")).is_err());
        assert!(validate_tree(folder.path(), Path::new("C:/outside")).is_err());
        assert_eq!(installation_root(&folder.path().join("Zed.exe"))?, None);
        Ok(())
    }
}
