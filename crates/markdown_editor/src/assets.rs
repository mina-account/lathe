use crate::{
    MarkdownEditor,
    protocol::{self, HostMessage, ImportedImage},
    settings::MarkdownEditorSettings,
};
use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::AsyncReadExt;
use gpui::{App, AppContext, ClipboardEntry, Context, PathPromptOptions, Task};
use language::ToOffset;
use path::rel_path::{RelPath, RelPathBuf};
use project::ProjectPath;
use settings::Settings;
use std::io::Read as _;
use std::{io::Cursor, path::PathBuf};

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
type LoadedImage = Result<(String, Vec<u8>)>;
pub(crate) enum ImageInput {
    Encoded(Vec<ImportedImage>),
    Picker,
    Clipboard,
    Paths(Vec<PathBuf>),
}

fn image_type(bytes: &[u8]) -> Result<(&'static str, &'static str)> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        bail!("Image exceeds the 4 MiB encoded limit");
    }
    let format = image::guess_format(bytes)?;
    let (extension, mime) = match format {
        image::ImageFormat::Png => ("png", "image/png"),
        image::ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        image::ImageFormat::WebP => ("webp", "image/webp"),
        image::ImageFormat::Gif => ("gif", "image/gif"),
        _ => bail!("Supported image types are PNG, JPEG, WebP, and GIF"),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits.clone());
    reader
        .decode()
        .context("Image data cannot be decoded within the 64 MiB limit")?;
    use image::{AnimationDecoder as _, ImageDecoder as _};
    if format == image::ImageFormat::Gif {
        let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
        decoder.set_limits(limits)?;
        validate_frames(decoder.into_frames())?;
    } else if format == image::ImageFormat::WebP {
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
        decoder.set_limits(limits)?;
        validate_frames(decoder.into_frames())?;
    }
    Ok((extension, mime))
}

fn validate_frames(frames: image::Frames<'_>) -> Result<()> {
    let mut decoded_bytes = 0usize;
    for (index, frame) in frames.enumerate() {
        if index >= 100 {
            bail!("Animated image exceeds 100 decoded frames");
        }
        decoded_bytes = decoded_bytes
            .checked_add(frame?.buffer().len())
            .context("Decoded image size overflow")?;
        if decoded_bytes > 64 * 1024 * 1024 {
            bail!("Animated image exceeds 64 MiB of decoded pixels");
        }
    }
    Ok(())
}

fn clipboard_image(bytes: &[u8]) -> Result<Vec<u8>> {
    if image::guess_format(bytes)? != image::ImageFormat::Bmp {
        image_type(bytes)?;
        return Ok(bytes.to_vec());
    }
    if bytes.len() > 64 * 1024 * 1024 {
        bail!("Clipboard image exceeds the 64 MiB decoded limit");
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Bmp);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut output = Cursor::new(Vec::new());
    reader
        .decode()?
        .write_to(&mut output, image::ImageFormat::Png)?;
    let bytes = output.into_inner();
    image_type(&bytes)?;
    Ok(bytes)
}

fn asset_document(
    project: gpui::Entity<project::Project>,
    document: ProjectPath,
    cx: &mut Context<MarkdownEditor>,
) -> Task<Result<(ProjectPath, gpui::Entity<project::Worktree>)>> {
    let Some(worktree) = project.read(cx).worktree_for_id(document.worktree_id, cx) else {
        return Task::ready(Err(anyhow::anyhow!("Owning worktree is unavailable")));
    };
    if !worktree.read(cx).is_single_file() {
        return Task::ready(Ok((document, worktree)));
    }
    let absolute = worktree.read(cx).abs_path().to_path_buf();
    let Some(parent) = absolute.parent() else {
        return Task::ready(Err(anyhow::anyhow!("Document parent is unavailable")));
    };
    let create = project.update(cx, |project, cx| {
        project.find_or_create_worktree(parent, false, cx)
    });
    cx.spawn(async move |_, cx| {
        let (worktree, _) = create.await?;
        let path = worktree.read_with(cx, |worktree, _| {
            let path = absolute.strip_prefix(worktree.abs_path().as_ref())?;
            Ok::<_, anyhow::Error>(RelPath::new(path, worktree.path_style())?.into_owned())
        })?;
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        Ok((
            ProjectPath {
                worktree_id,
                path: path.into(),
            },
            worktree,
        ))
    })
}

fn normalize_path(base: &str, supplied: &str) -> Result<RelPathBuf> {
    if supplied.contains(':')
        || supplied.starts_with('/')
        || supplied.starts_with('\\')
        || supplied.chars().any(char::is_control)
    {
        bail!("Asset path must stay inside the owning worktree");
    }
    let combined = if base.is_empty() {
        supplied.to_owned()
    } else {
        format!("{base}/{supplied}")
    };
    Ok(RelPath::new(
        std::path::Path::new(&combined.replace('\\', "/")),
        path::PathStyle::Unix,
    )?
    .into_owned())
}

fn destination(document: &ProjectPath, folder: &str) -> Result<RelPathBuf> {
    let (base, folder) = if let Some(folder) = folder.strip_prefix("${projectRoot}/") {
        ("", folder)
    } else if folder == "${projectRoot}" {
        ("", "")
    } else {
        (
            document
                .path
                .parent()
                .unwrap_or(RelPath::empty())
                .as_unix_str(),
            folder,
        )
    };
    normalize_path(base, folder)
}

fn relative_link(document: &ProjectPath, image: &RelPath) -> String {
    let parent = document.path.parent().unwrap_or(RelPath::empty());
    let parent: Vec<_> = parent.components().collect();
    let target: Vec<_> = image.components().collect();
    let common = parent
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n("..", parent.len() - common)
        .chain(target.into_iter().skip(common))
        .map(|part| urlencoding::encode(part).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl MarkdownEditor {
    pub(crate) fn request_asset(
        &mut self,
        request: u64,
        url: String,
        service: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.active_asset_requests >= 16 {
            bail!("Too many pending Markdown resources");
        }
        let project = self
            .source
            .read(cx)
            .project()
            .cloned()
            .context("Image needs an owning project")?;
        let load: Task<Result<(String, Vec<u8>)>> = if service {
            let configured = MarkdownEditorSettings::get_global(cx)
                .plantuml_service
                .clone()
                .context("PlantUML requires an explicitly configured network service")?;
            let configured = http_client::Url::parse(&configured)?;
            if !["http", "https"].contains(&configured.scheme())
                || !configured.username().is_empty()
                || configured.password().is_some()
                || configured.query().is_some()
                || configured.fragment().is_some()
            {
                bail!(
                    "PlantUML service must be an HTTP(S) server root without credentials, query, or fragment"
                );
            }
            let endpoint = format!("{}/svg/~1{url}", configured.as_str().trim_end_matches('/'));
            let client = project.read(cx).client().http_client();
            cx.background_spawn(async move {
                let mut response = client
                    .get(&endpoint, http_client::AsyncBody::empty(), false)
                    .await?;
                if !response.status().is_success() {
                    bail!("PlantUML service returned {}", response.status());
                }
                let mut bytes = Vec::new();
                response
                    .body_mut()
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                if bytes.len() > 1024 * 1024 {
                    bail!("PlantUML response exceeds 1 MiB");
                }
                Ok(("image/svg+xml".into(), bytes))
            })
        } else {
            let document = self
                .buffer
                .read(cx)
                .file()
                .map(|file| ProjectPath::from_file(file.as_ref(), cx))
                .context("Save the document before loading relative images")?;
            let standalone = project
                .read(cx)
                .worktree_for_id(document.worktree_id, cx)
                .is_some_and(|worktree| worktree.read(cx).is_single_file());
            if !standalone {
                image_resource_path(&document, &url, &project, cx)?;
            }
            let resolve = asset_document(project.clone(), document, cx);
            cx.spawn(async move |_, cx| {
                let (document, _worktree) = resolve.await?;
                let path = project.read_with(cx, |_, cx| {
                    image_resource_path(&document, &url, &project, cx)
                })?;
                let load = project.update(cx, |project, cx| {
                    project.load_binary_resource(path, MAX_IMAGE_BYTES, cx)
                });
                let bytes = load.await?;
                let (_, mime) = image_type(&bytes)?;
                Ok((mime.into(), bytes))
            })
        };
        self.active_asset_requests += 1;
        let session = self.protocol.session.clone();
        let timeout = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            let result = match futures::future::select(load, timeout).await {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right(_) => {
                    Err(anyhow::anyhow!("Markdown resource timed out"))
                }
            };
            if let Err(error) = this.update(cx, |this, _| {
                this.active_asset_requests = this.active_asset_requests.saturating_sub(1);
                if this.protocol.session != session {
                    return Ok(());
                }
                let (mime, bytes, error) = match result {
                    Ok((mime, bytes)) => (mime, STANDARD.encode(bytes), None),
                    Err(error) => (String::new(), String::new(), Some(error.to_string())),
                };
                this.send(HostMessage::Resource {
                    request,
                    mime,
                    bytes,
                    error,
                })
            }) {
                log::debug!("Markdown resource item closed: {error:#}");
            }
        })
        .detach();
        Ok(())
    }

    pub(crate) fn import_images(
        &mut self,
        revision: u64,
        position: usize,
        input: ImageInput,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if !self.asset_active(cx)
            || self.protocol.draft.is_some()
            || revision != self.protocol.revision
            || self.importing_images
        {
            bail!("Flush writable document input before importing images");
        }
        let offset = protocol::utf16_to_byte(self.protocol.current(), position)?;
        let anchor = self.buffer.read(cx).snapshot().anchor_after(offset);
        let needs_save = self.buffer.read(cx).file().is_none();
        let window = self.window;
        let save: Option<Task<Result<()>>> = if needs_save {
            let item = cx.entity();
            Some(cx.spawn(async move |_, cx| {
                let task = window.update(cx, |_, window, cx| {
                    let workspace = workspace::Workspace::for_window(window, cx)
                        .context("Workspace is unavailable for Save As")?;
                    Ok::<_, anyhow::Error>(workspace.update(cx, |workspace, cx| {
                        workspace.activate_item(&item, true, true, window, cx);
                        workspace.save_active_item(workspace::SaveIntent::SaveAs, window, cx)
                    }))
                })??;
                task.await?;
                Ok(())
            }))
        } else {
            None
        };
        self.importing_images = true;
        self.asset_started(cx);
        cx.spawn(async move |this, cx| {
            let result: Result<()> = async {
                if let Some(save) = save {
                    save.await?;
                }
                let load = this.update(cx, |this, cx| {
                    if this.buffer.read(cx).file().is_none() {
                        bail!("Save As was cancelled; no image was inserted");
                    }
                    let filesystem = this
                        .source
                        .read(cx)
                        .project()
                        .context("Project unavailable")?
                        .read(cx)
                        .fs()
                        .clone();
                    Ok::<_, anyhow::Error>(load_image_input(input, filesystem, cx))
                })??;
                let images = load.await?;
                if images.len() > 16 {
                    bail!("Import at most 16 images in one batch");
                }
                for image in images {
                    let result: Result<()> = async {
                        let (name, bytes) = image?;
                        let (extension, _) = image_type(&bytes)?;
                        let (project, document, folder) = this.read_with(cx, |this, cx| {
                            if !this.asset_active(cx) {
                                bail!("Document closed or became read-only during import");
                            }
                            let document = this
                                .buffer
                                .read(cx)
                                .file()
                                .map(|file| ProjectPath::from_file(file.as_ref(), cx))
                                .context("Save As was cancelled; no image was inserted")?;
                            Ok((
                                this.source
                                    .read(cx)
                                    .project()
                                    .cloned()
                                    .context("Project unavailable")?,
                                document,
                                MarkdownEditorSettings::get_global(cx)
                                    .image_save_folder
                                    .clone(),
                            ))
                        })??;
                        let original_document = document;
                        let resolve = this.update(cx, |_, cx| {
                            asset_document(project.clone(), original_document.clone(), cx)
                        })?;
                        let (document, worktree) = resolve.await?;
                        let folder = destination(&document, &folder)?;
                        let create = worktree.update(cx, |worktree, cx| {
                            worktree.create_directory_confined(folder.clone().into(), cx)
                        });
                        create.await?;
                        let stem = name
                            .rsplit(['\\', '/'])
                            .next()
                            .unwrap_or("image")
                            .split('.')
                            .next()
                            .unwrap_or("image")
                            .chars()
                            .filter(|character| {
                                character.is_alphanumeric() || [' ', '-', '_'].contains(character)
                            })
                            .take(80)
                            .collect::<String>();
                        let filename = format!(
                            "{}-{}.{}",
                            if stem.is_empty() { "image" } else { &stem },
                            uuid::Uuid::new_v4(),
                            extension
                        );
                        let image_path = folder.join(RelPath::from_unix_str(&filename)?.as_ref());
                        let write = worktree.update(cx, |worktree, cx| {
                            worktree.create_entry_exclusive(image_path.clone().into(), bytes, cx)
                        });
                        write.await?;
                        this.update(cx, |this, cx| {
                            if !this.asset_active(cx) {
                                bail!("Document closed or became read-only after image write");
                            }
                            let current = this
                                .buffer
                                .read(cx)
                                .file()
                                .map(|file| ProjectPath::from_file(file.as_ref(), cx))
                                .context("Document path is unavailable")?;
                            if current.worktree_id != original_document.worktree_id {
                                bail!("Document moved to another worktree during image write");
                            }
                            let current = if original_document.path.is_empty() {
                                let owning = project
                                    .read(cx)
                                    .worktree_for_id(current.worktree_id, cx)
                                    .context("Document worktree is unavailable")?;
                                let absolute = owning.read(cx).abs_path();
                                let relative =
                                    absolute.strip_prefix(worktree.read(cx).abs_path().as_ref())
                                        .context("Document moved outside the asset worktree during image write")?;
                                ProjectPath {
                                    worktree_id: document.worktree_id,
                                    path: RelPath::new(relative, worktree.read(cx).path_style())?
                                        .into_owned()
                                        .into(),
                                }
                            } else {
                                current
                            };
                            let link =
                                format!("![image]({})", relative_link(&current, &image_path));
                            let snapshot = this.buffer.read(cx).snapshot();
                            let offset = anchor.to_offset(&snapshot);
                            if !protocol::within_document_budget(&(snapshot.text() + &link)) {
                                bail!("Image reference exceeds the document limit");
                            }
                            this.buffer.update(cx, |buffer, cx| {
                                buffer.finalize_last_transaction();
                                buffer.edit([(offset..offset, link)], None, cx);
                                buffer.finalize_last_transaction();
                            });
                            Ok(())
                        })??;
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        this.update(cx, |this, cx| this.asset_notice(error.to_string(), cx))?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = this.update(cx, |this, cx| {
                this.importing_images = false;
                if let Err(error) = result {
                    this.asset_notice(error.to_string(), cx);
                }
            }) {
                log::debug!("Markdown import item closed: {error:#}");
            }
        })
        .detach();
        Ok(())
    }
}

fn load_image_input(
    input: ImageInput,
    filesystem: std::sync::Arc<dyn project::Fs>,
    cx: &mut Context<MarkdownEditor>,
) -> Task<Result<Vec<LoadedImage>>> {
    match input {
        ImageInput::Encoded(images) => Task::ready(Ok(images
            .into_iter()
            .map(|image| {
                STANDARD
                    .decode(image.bytes)
                    .map(|bytes| (image.name, bytes))
                    .map_err(Into::into)
            })
            .collect())),
        ImageInput::Clipboard => {
            let mut images = Vec::new();
            let mut paths = Vec::new();
            for entry in cx
                .read_from_clipboard()
                .map(|item| item.entries)
                .unwrap_or_default()
            {
                match entry {
                    ClipboardEntry::Image(image) => {
                        images.push(Ok(("clipboard".into(), image.bytes().to_vec())))
                    }
                    ClipboardEntry::ExternalPaths(external) => {
                        paths.extend(external.paths().iter().cloned())
                    }
                    _ => {}
                }
            }
            cx.background_spawn(async move {
                let mut images = images
                    .into_iter()
                    .map(|image: LoadedImage| {
                        image.and_then(|(name, bytes)| {
                            clipboard_image(&bytes).map(|bytes| (name, bytes))
                        })
                    })
                    .collect::<Vec<_>>();
                for path in paths {
                    images.push(load_image_path(&filesystem, path).await);
                }
                Ok(images)
            })
        }
        ImageInput::Paths(paths) => cx.background_spawn(async move {
            let mut images = Vec::new();
            for path in paths {
                images.push(load_image_path(&filesystem, path).await);
            }
            Ok(images)
        }),
        ImageInput::Picker => {
            let prompt = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("Select images".into()),
            });
            cx.background_spawn(async move {
                let mut images = Vec::new();
                if let Some(paths) = prompt.await?? {
                    for path in paths {
                        images.push(load_image_path(&filesystem, path).await);
                    }
                }
                Ok(images)
            })
        }
    }
}

async fn load_image_path(
    filesystem: &std::sync::Arc<dyn project::Fs>,
    path: PathBuf,
) -> LoadedImage {
    let mut bytes = Vec::new();
    std::io::Read::take(
        filesystem.open_sync(&path).await?,
        MAX_IMAGE_BYTES as u64 + 1,
    )
    .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!("Image exceeds the 4 MiB encoded limit");
    }
    Ok((
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into()),
        bytes,
    ))
}

fn image_resource_path(
    document: &ProjectPath,
    url: &str,
    project: &gpui::Entity<project::Project>,
    cx: &App,
) -> Result<ProjectPath> {
    let path = url.split('#').next().unwrap_or(url);
    let decoded = urlencoding::decode(path)?;
    let worktree = project
        .read(cx)
        .worktree_for_id(document.worktree_id, cx)
        .context("Owning worktree unavailable")?;
    let root = worktree
        .read(cx)
        .abs_path()
        .to_string_lossy()
        .replace('\\', "/");
    let decoded = decoded.replace('\\', "/");
    let relative = if decoded.starts_with('/') || decoded.as_bytes().get(1) == Some(&b':') {
        let path = decoded
            .strip_prefix(&(root.trim_end_matches('/').to_owned() + "/"))
            .context("Absolute image is outside the owning worktree")?;
        normalize_path("", path)?
    } else {
        normalize_path(
            document
                .path
                .parent()
                .unwrap_or(RelPath::empty())
                .as_unix_str(),
            &decoded,
        )?
    };
    Ok(ProjectPath {
        worktree_id: document.worktree_id,
        path: relative.into(),
    })
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::{BrowserSurface, FakeBrowserSurface};
    use futures::StreamExt as _;
    use gpui::{TestAppContext, UpdateGlobal, VisualTestContext};
    use std::{cell::RefCell, rc::Rc};
    use util::{path, rel_path::rel_path};

    struct Recording(Rc<RefCell<Vec<HostMessage>>>);
    impl BrowserSurface for Recording {
        fn send(&mut self, message: &HostMessage) -> Result<()> {
            self.0.borrow_mut().push(message.clone());
            Ok(())
        }
        fn poll(&mut self) -> Result<Vec<protocol::ClientMessage>> {
            Ok(Vec::new())
        }
        fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn png() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("encode fixture");
        output.into_inner()
    }

    #[gpui::test]
    async fn shared_resources_authorize_requested_and_resolved_private_paths(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let project = visual.read_with(cx, |item, cx| {
            item.source.read(cx).project().expect("project").clone()
        });
        let worktree_id = visual.read_with(cx, |item, cx| {
            item.buffer.read(cx).file().expect("file").worktree_id(cx)
        });
        let defaults = [
            ".env",
            ".env.test",
            "fixture.pem",
            "fixture.key",
            "fixture.cert",
            "fixture.crt",
            "secrets.yml",
        ];
        for (index, target) in defaults
            .into_iter()
            .chain(["confidential/fixture.txt", "fixtures/custom.conf"])
            .enumerate()
        {
            let custom = index >= defaults.len();
            if custom {
                cx.update(|_, cx| {
                    settings::SettingsStore::update_global(cx, |store, cx| {
                        store
                            .set_user_settings(
                                r#"{"private_files":["confidential","**/*.conf"]}"#,
                                cx,
                            )
                            .result()
                            .expect("custom private patterns");
                    })
                });
            }
            let absolute = std::path::Path::new(path!("/project")).join(target);
            filesystem
                .create_dir(absolute.parent().expect("parent"))
                .await
                .expect("fixture directory");
            filesystem
                .write(&absolute, b"harmless fixture")
                .await
                .expect("fixture bytes");
            let alias = format!("alias-{index}.bin");
            filesystem
                .insert_symlink(
                    std::path::Path::new(path!("/project")).join(&alias),
                    std::path::PathBuf::from(target),
                )
                .await;
            for requested in [target.to_owned(), alias] {
                let result = project
                    .update(cx, |project, cx| {
                        project.load_shared_binary_resource(
                            (worktree_id, rel_path(&requested)).into(),
                            1024,
                            cx,
                        )
                    })
                    .await;
                assert!(
                    result.is_err(),
                    "private request must be denied: {requested}"
                );
            }
            assert_eq!(
                project
                    .update(cx, |project, cx| project.load_binary_resource(
                        (worktree_id, rel_path(target)).into(),
                        1024,
                        cx
                    ))
                    .await
                    .expect("owner may read private fixture"),
                b"harmless fixture"
            );
        }
        filesystem
            .write(path!("/project/public.bin").as_ref(), b"public fixture")
            .await
            .expect("public fixture");
        filesystem
            .insert_symlink(
                path!("/project/public-alias.bin"),
                std::path::PathBuf::from("public.bin"),
            )
            .await;
        filesystem
            .insert_symlink(
                path!("/project/private-alias.conf"),
                std::path::PathBuf::from("public.bin"),
            )
            .await;
        for requested in ["public.bin", "public-alias.bin"] {
            assert_eq!(
                project
                    .update(cx, |project, cx| project.load_shared_binary_resource(
                        (worktree_id, rel_path(requested)).into(),
                        1024,
                        cx
                    ))
                    .await
                    .expect("permitted shared fixture"),
                b"public fixture"
            );
        }
        assert!(
            project
                .update(cx, |project, cx| project.load_shared_binary_resource(
                    (worktree_id, rel_path("private-alias.conf")).into(),
                    1024,
                    cx
                ))
                .await
                .is_err()
        );
    }

    #[gpui::test]
    async fn image_import_rejects_escaped_directories_before_any_outside_write(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        filesystem
            .insert_tree(path!("/outside"), serde_json::json!({}))
            .await;
        filesystem
            .insert_symlink(path!("/project/docs/escape"), path!("/outside").into())
            .await;
        for folder in [
            "escape/nested/deeper",
            "${projectRoot}/docs/escape/root-nested",
        ] {
            cx.update(|_, cx| {
                settings::SettingsStore::update_global(cx, |store, cx| {
                    store
                        .set_user_settings(
                            &serde_json::json!({"markdown_editor":{"image_save_folder":folder}})
                                .to_string(),
                            cx,
                        )
                        .result()
                        .expect("destination setting");
                })
            });
            visual
                .update(cx, |item, cx| {
                    item.import_images(
                        0,
                        0,
                        ImageInput::Encoded(vec![ImportedImage {
                            name: "fixture.png".into(),
                            bytes: STANDARD.encode(png()),
                        }]),
                        cx,
                    )
                })
                .expect("import request");
            cx.run_until_parked();
            assert_eq!(
                filesystem
                    .read_dir(path!("/outside").as_ref())
                    .await
                    .expect("outside fixture")
                    .collect::<Vec<_>>()
                    .await
                    .len(),
                0,
                "no outside directory or file may be created"
            );
            assert_eq!(
                visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
                "Before\n\nTail"
            );
            assert!(visual.read_with(cx, |item, _| item.error().is_some()));
        }
        filesystem
            .insert_symlink(
                path!("/project/docs/safe-alias"),
                std::path::PathBuf::from("assets"),
            )
            .await;
        for folder in [
            "new/nested",
            "safe-alias/nested",
            "${projectRoot}/root/nested",
        ] {
            cx.update(|_, cx| {
                settings::SettingsStore::update_global(cx, |store, cx| {
                    store
                        .set_user_settings(
                            &serde_json::json!({"markdown_editor":{"image_save_folder":folder}})
                                .to_string(),
                            cx,
                        )
                        .result()
                        .expect("safe destination setting");
                })
            });
            visual
                .update(cx, |item, cx| {
                    item.import_images(
                        item.protocol.revision,
                        0,
                        ImageInput::Encoded(vec![ImportedImage {
                            name: "fixture.png".into(),
                            bytes: STANDARD.encode(png()),
                        }]),
                        cx,
                    )
                })
                .expect("safe import request");
            cx.run_until_parked();
            assert!(visual.read_with(cx, |item, cx| {
                item.buffer
                    .read(cx)
                    .snapshot()
                    .text()
                    .starts_with("![image](")
            }));
            visual.update(cx, |item, cx| {
                item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
            });
            cx.run_until_parked();
            assert_eq!(
                visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
                "Before\n\nTail"
            );
        }
    }

    #[gpui::test]
    async fn native_bmp_clipboard_imports_as_validated_png(cx: &mut TestAppContext) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut encoded, image::ImageFormat::Bmp)
            .expect("BMP clipboard fixture");
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
                gpui::ImageFormat::Bmp,
                encoded.into_inner(),
            )))
        });
        visual
            .update(cx, |item, cx| {
                item.import_images(0, 0, ImageInput::Clipboard, cx)
            })
            .expect("clipboard import");
        cx.run_until_parked();
        let source = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        assert!(source.starts_with("![image](assets/clipboard-"), "{source}");
        let files = filesystem
            .read_dir(path!("/project/docs/assets").as_ref())
            .await
            .expect("assets")
            .collect::<Vec<_>>()
            .await;
        let path = files
            .into_iter()
            .find_map(|entry| {
                entry.ok().filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("clipboard-"))
                })
            })
            .expect("clipboard file");
        let mut bytes = Vec::new();
        filesystem
            .open_sync(&path)
            .await
            .expect("image")
            .read_to_end(&mut bytes)
            .expect("read image");
        assert_eq!(
            image_type(&bytes).expect("validated PNG"),
            ("png", "image/png")
        );
        assert_eq!(image::load_from_memory(&bytes).expect("decode").width(), 2);
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
        });
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
        assert!(filesystem.is_file(&path).await);
    }

    #[gpui::test]
    async fn bounded_image_wire_batch_decodes_and_keeps_native_document_editable(
        cx: &mut TestAppContext,
    ) {
        let (visual, _, cx) = fixture(cx).await;
        let mut bytes = png();
        bytes.resize(MAX_IMAGE_BYTES, 0);
        image_type(&bytes).expect("valid padded PNG");
        visual.update(cx, |item, cx| {
            let message = protocol::ClientMessage::ImportImages {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                base_revision: item.protocol.revision,
                position: 0,
                images: (0..2)
                    .map(|index| ImportedImage {
                        name: format!("large-{index}.png"),
                        bytes: STANDARD.encode(&bytes),
                    })
                    .collect(),
            };
            let encoded = serde_json::to_string(&message).expect("wire image batch");
            assert!(encoded.len() <= protocol::MAX_MESSAGE_BYTES);
            let message = protocol::decode(
                &protocol::application_url(&item.protocol.session, item.protocol.document),
                &encoded,
                &item.protocol.session,
                item.protocol.document,
            )
            .expect("actual bridge decoder");
            item.receive(message, cx).expect("native import");
        });
        cx.run_until_parked();
        visual.update(cx, |item, cx| {
            assert_eq!(
                item.buffer
                    .read(cx)
                    .snapshot()
                    .text()
                    .matches("![image]")
                    .count(),
                2
            );
            assert!(item.asset_active(cx));
            item.buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "Still editable ")], None, cx);
            });
            assert!(
                item.buffer
                    .read(cx)
                    .snapshot()
                    .text()
                    .starts_with("Still editable ")
            );
        });
    }

    #[gpui::test]
    async fn standalone_document_import_and_resource_use_its_parent_directory(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (first, filesystem, cx) = fixture(cx).await;
        filesystem
            .insert_tree(
                path!("/standalone"),
                serde_json::json!({"doc.md":"Standalone", "assets":{}}),
            )
            .await;
        filesystem
            .write(path!("/standalone/assets/existing.png").as_ref(), &png())
            .await
            .expect("existing image");
        let project = first.read_with(cx, |item, cx| {
            item.source.read(cx).project().expect("project").clone()
        });
        let (worktree, relative) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/standalone/doc.md"), true, cx)
            })
            .await
            .expect("standalone worktree");
        assert!(worktree.read_with(cx, |worktree, _| worktree.is_single_file()));
        assert!(relative.is_empty());
        let buffer = project
            .update(cx, |project, cx| {
                project.open_buffer((worktree.read(cx).id(), relative), cx)
            })
            .await
            .expect("standalone buffer");
        let visual = cx.update(|window, cx| {
            let source = cx.new(|cx| {
                editor::Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
            });
            cx.new(|cx| MarkdownEditor::new(source, buffer.clone(), window, cx))
        });
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("attach");
        visual
            .update(cx, |item, cx| {
                item.request_asset(55, "assets/existing.png".into(), false, cx)
            })
            .expect("request");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message| matches!(message, HostMessage::Resource{request:55, error:None, bytes, ..} if STANDARD.decode(bytes).expect("base64")==png())));
        visual
            .update(cx, |item, cx| {
                item.request_asset(56, "../private.png".into(), false, cx)
            })
            .expect("confined standalone request");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message| matches!(
            message,
            HostMessage::Resource {
                request: 56,
                error: Some(_),
                ..
            }
        )));
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    5,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "new".into(),
                        bytes: STANDARD.encode(png()),
                    }]),
                    cx,
                )
            })
            .expect("import");
        cx.run_until_parked();
        let text = buffer.read_with(cx, |buffer, _| buffer.snapshot().text());
        assert!(text.starts_with("Stand![image](assets/new-"), "{text}");
        assert_eq!(
            filesystem
                .read_dir(path!("/standalone/assets").as_ref())
                .await
                .expect("assets")
                .collect::<Vec<_>>()
                .await
                .len(),
            2
        );
        project
            .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))
            .await
            .expect("save image source");
        assert_eq!(
            filesystem
                .load(path!("/standalone/doc.md").as_ref())
                .await
                .expect("saved source"),
            text
        );
        buffer.update(cx, |buffer, cx| buffer.undo(cx));
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.snapshot().text()),
            "Standalone"
        );
    }

    async fn fixture(
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<MarkdownEditor>,
        std::sync::Arc<project::FakeFs>,
        &mut VisualTestContext,
    ) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem.insert_tree(path!("/project"),serde_json::json!({"docs":{"doc.md":"Before\n\nTail","assets":{"same.png":"existing"}},"other":{"doc.md":"Other"}})).await;
        let project = project::Project::test(filesystem.clone(), None, cx).await;
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/project"), true, cx)
            })
            .await
            .expect("worktree");
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        let buffer = project
            .update(cx, |project, cx| {
                project.open_buffer((worktree_id, rel_path("docs/doc.md")), cx)
            })
            .await
            .expect("buffer");
        let (visual, cx) = cx.add_window_view(|window, cx| {
            let source =
                cx.new(|cx| editor::Editor::for_buffer(buffer.clone(), Some(project), window, cx));
            MarkdownEditor::new(source, buffer, window, cx)
        });
        visual
            .update(cx, |item, cx| {
                item.attach(Box::<FakeBrowserSurface>::default(), cx)
            })
            .expect("attach");
        (visual, filesystem, cx)
    }

    #[gpui::test]
    async fn image_batch_writes_before_anchored_insertion_and_keeps_partial_failures_visible(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    8,
                    ImageInput::Encoded(vec![
                        ImportedImage {
                            name: "same 中文.png".into(),
                            bytes: STANDARD.encode(png()),
                        },
                        ImportedImage {
                            name: "broken.png".into(),
                            bytes: STANDARD.encode(b"invalid"),
                        },
                        ImportedImage {
                            name: "same 中文.png".into(),
                            bytes: STANDARD.encode(png()),
                        },
                    ]),
                    cx,
                )
            })
            .expect("start batch");
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "Native prefix\n")], None, cx)
            })
        });
        cx.run_until_parked();
        let text = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        assert!(
            text.starts_with("Native prefix\nBefore\n\n![image](assets/same%20%E4%B8%AD%E6%96%87-"),
            "{text}"
        );
        assert_eq!(text.matches("![image]").count(), 2);
        assert!(text.ends_with("Tail"));
        assert_eq!(
            filesystem
                .load(path!("/project/docs/assets/same.png").as_ref())
                .await
                .expect("existing"),
            "existing"
        );
        for destination in text
            .split("![image](")
            .skip(1)
            .filter_map(|link| link.split(')').next())
        {
            let path = PathBuf::from(path!("/project/docs"))
                .join(urlencoding::decode(destination).expect("decode").as_ref());
            assert_eq!(
                filesystem.load_bytes(&path).await.expect("persisted image"),
                png()
            );
        }
        assert!(visual.read_with(cx, |item, _| item.error().is_some()));
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
        });
        cx.run_until_parked();
        assert!(
            filesystem
                .read_dir(path!("/project/docs/assets").as_ref())
                .await
                .expect("assets")
                .collect::<Vec<_>>()
                .await
                .len()
                >= 3
        );
    }

    #[gpui::test]
    async fn image_import_rejects_readonly_and_close_without_inserting_a_link(
        cx: &mut TestAppContext,
    ) {
        let (visual, _, cx) = fixture(cx).await;
        visual.update(cx, |item, cx| {
            item.source
                .update(cx, |source, _| source.set_read_only(true))
        });
        assert!(
            visual
                .update(cx, |item, cx| item.import_images(
                    0,
                    0,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "image".into(),
                        bytes: STANDARD.encode(png())
                    }]),
                    cx
                ))
                .is_err()
        );
        visual.update(cx, |item, cx| {
            item.source
                .update(cx, |source, _| source.set_read_only(false))
        });
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    0,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "image".into(),
                        bytes: STANDARD.encode(png()),
                    }]),
                    cx,
                )
            })
            .expect("start");
        visual.update(cx, |item, _| item.stop_browser_pump());
        cx.run_until_parked();
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
    }

    #[gpui::test]
    async fn image_resource_reads_actual_project_bytes_and_reports_confinement_errors(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        filesystem
            .write(path!("/project/docs/assets/中 #.png").as_ref(), &png())
            .await
            .expect("fixture image");
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("attach recording");
        visual
            .update(cx, |item, cx| {
                item.request_asset(4, "assets/%E4%B8%AD%20%23.png".into(), false, cx)
            })
            .expect("resource request");
        cx.run_until_parked();
        let recorded = messages.borrow();
        assert!(recorded.iter().any(|message|matches!(message,HostMessage::Resource{request:4,mime,bytes,error:None} if mime=="image/png" && STANDARD.decode(bytes).expect("base64")==png())));
        drop(recorded);
        let absolute = visual.read_with(cx, |item, cx| {
            let document =
                ProjectPath::from_file(item.buffer.read(cx).file().expect("document").as_ref(), cx);
            let project = item.source.read(cx).project().expect("project");
            let worktree = project
                .read(cx)
                .worktree_for_id(document.worktree_id, cx)
                .expect("worktree");
            format!(
                "{}/docs/assets/%E4%B8%AD%20%23.png",
                worktree
                    .read(cx)
                    .abs_path()
                    .to_string_lossy()
                    .replace('\\', "/")
            )
        });
        visual
            .update(cx, |item, cx| item.request_asset(7, absolute, false, cx))
            .expect("scoped native absolute image");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message|matches!(message,HostMessage::Resource{request:7,error:None,bytes,..} if STANDARD.decode(bytes).expect("base64")==png())));
        assert!(
            visual
                .update(cx, |item, cx| item.request_asset(
                    5,
                    "../../private.png".into(),
                    false,
                    cx
                ))
                .is_err()
        );
        assert!(
            visual
                .update(cx, |item, cx| item.request_asset(
                    6,
                    "https://example.com/image.png".into(),
                    false,
                    cx
                ))
                .is_err()
        );
    }

    #[test]
    fn destinations_and_links_resolve_relative_to_the_owning_document() {
        let document = ProjectPath {
            worktree_id: project::WorktreeId::from_proto(7),
            path: rel_path("docs/nested/doc.md").into(),
        };
        assert_eq!(
            destination(&document, "assets")
                .expect("relative")
                .as_unix_str(),
            "docs/nested/assets"
        );
        assert_eq!(
            destination(&document, "${projectRoot}/assets")
                .expect("root")
                .as_unix_str(),
            "assets"
        );
        assert_eq!(
            relative_link(&document, rel_path("assets/中 #%().png")),
            "../../assets/%E4%B8%AD%20%23%25%28%29.png"
        );
        assert!(destination(&document, "../../../private").is_err());
        assert!(destination(&document, "C:\\private").is_err());
    }

    #[test]
    fn decoded_image_limits_reject_unsupported_corrupt_and_excessive_animation() {
        assert!(image_type(b"<svg onload='evil()'/>").is_err());
        assert!(image_type(&vec![0; MAX_IMAGE_BYTES + 1]).is_err());
        assert!(image_type(&png()[..24]).is_err());
        let mut encoded = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut encoded);
            for _ in 0..101 {
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::new(1, 1)))
                    .expect("animation frame");
            }
        }
        assert!(
            image_type(&encoded).is_err(),
            "Animated images must obey a decoded frame limit"
        );
    }

    #[gpui::test]
    async fn project_root_images_use_the_document_worktree_among_multiple_roots(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (first, filesystem, cx) = fixture(cx).await;
        filesystem
            .insert_tree(
                path!("/another"),
                serde_json::json!({"docs":{"doc.md":"Second"}}),
            )
            .await;
        let project = first.read_with(cx, |item, cx| {
            item.source.read(cx).project().expect("project").clone()
        });
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/another"), true, cx)
            })
            .await
            .expect("second worktree");
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        let buffer = project
            .update(cx, |project, cx| {
                project.open_buffer((worktree_id, rel_path("docs/doc.md")), cx)
            })
            .await
            .expect("second buffer");
        let second = cx.update(|window, cx| {
            let source = cx.new(|cx| {
                editor::Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
            });
            cx.new(|cx| MarkdownEditor::new(source, buffer.clone(), window, cx))
        });
        second
            .update(cx, |item, cx| {
                item.attach(Box::<FakeBrowserSurface>::default(), cx)
            })
            .expect("second attach");
        cx.update(|_, cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{"markdown_editor":{"image_save_folder":"${projectRoot}/assets"}}"#,
                        cx,
                    )
                    .result()
                    .expect("root setting")
            })
        });
        second
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    6,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "root".into(),
                        bytes: STANDARD.encode(png()),
                    }]),
                    cx,
                )
            })
            .expect("root import");
        cx.run_until_parked();
        let text = buffer.read_with(cx, |buffer, _| buffer.snapshot().text());
        assert!(text.starts_with("Second![image](../assets/"));
        let images = filesystem
            .read_dir(path!("/another/assets").as_ref())
            .await
            .expect("owning root assets")
            .collect::<Vec<_>>()
            .await;
        assert_eq!(images.len(), 1);
        assert!(!filesystem.is_dir(path!("/project/assets").as_ref()).await);
    }

    #[gpui::test]
    async fn renamed_document_resources_reset_without_mutating_source(cx: &mut TestAppContext) {
        let (visual, _, cx) = fixture(cx).await;
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("recording");
        let (project, document) = visual.read_with(cx, |item, cx| {
            (
                item.source.read(cx).project().expect("project").clone(),
                ProjectPath::from_file(item.buffer.read(cx).file().expect("file").as_ref(), cx),
            )
        });
        let entry = project.read_with(cx, |project, cx| {
            project.entry_for_path(&document, cx).expect("entry").id
        });
        project
            .update(cx, |project, cx| {
                project.rename_entry(
                    entry,
                    (document.worktree_id, rel_path("other/renamed.md")).into(),
                    cx,
                )
            })
            .await
            .expect("rename");
        cx.run_until_parked();
        assert!(
            messages
                .borrow()
                .iter()
                .any(|message| matches!(message, HostMessage::ResourcesReset))
        );
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    item.protocol.revision,
                    8,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "renamed".into(),
                        bytes: STANDARD.encode(png()),
                    }]),
                    cx,
                )
            })
            .expect("renamed import");
        cx.run_until_parked();
        assert!(visual.read_with(cx, |item, cx| {
            item.buffer
                .read(cx)
                .snapshot()
                .text()
                .starts_with("Before\n\n![image](assets/")
        }));
    }

    #[gpui::test]
    async fn image_picker_clipboard_and_native_paths_use_the_same_project_import(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        filesystem
            .write(path!("/project/incoming.png").as_ref(), &png())
            .await
            .expect("input image");
        visual
            .update(cx, |item, cx| {
                item.import_images(item.protocol.revision, 0, ImageInput::Picker, cx)
            })
            .expect("picker import");
        cx.run_until_parked();
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.multiple && options.files && !options.directories);
            Some(vec![PathBuf::from(path!("/project/incoming.png"))])
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                png(),
            )))
        });
        visual
            .update(cx, |item, cx| {
                item.import_images(item.protocol.revision, 0, ImageInput::Clipboard, cx)
            })
            .expect("clipboard import");
        cx.run_until_parked();
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    item.protocol.revision,
                    0,
                    ImageInput::Paths(vec![
                        PathBuf::from(path!("/project/missing.png")),
                        PathBuf::from(path!("/project/incoming.png")),
                    ]),
                    cx,
                )
            })
            .expect("path batch");
        cx.run_until_parked();
        assert_eq!(
            visual.read_with(cx, |item, cx| item
                .buffer
                .read(cx)
                .snapshot()
                .text()
                .matches("![image]")
                .count()),
            3
        );
        let before = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        visual
            .update(cx, |item, cx| {
                item.import_images(item.protocol.revision, 0, ImageInput::Picker, cx)
            })
            .expect("cancel picker");
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            before
        );
    }

    #[gpui::test]
    async fn configured_plantuml_uses_bounded_host_http_and_reports_service_errors(
        cx: &mut TestAppContext,
    ) {
        use http_client::HttpClient as _;
        let (visual, _, cx) = fixture(cx).await;
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("recording");
        assert!(
            visual
                .update(cx, |item, cx| item.request_asset(
                    30,
                    "Abcd_012-".into(),
                    true,
                    cx
                ))
                .is_err()
        );
        cx.update(|_,cx|settings::SettingsStore::update_global(cx,|store,cx|store.set_user_settings(r#"{"markdown_editor":{"plantuml_service":"https://service.invalid/plantuml"}}"#,cx).result().expect("service setting")));
        visual.read_with(cx,|item,cx| {
            item.source.read(cx).project().expect("project").read(cx).client().http_client().as_fake().replace_handler(|_,request| async move {
                assert_eq!(request.uri().to_string(),"https://service.invalid/plantuml/svg/~1Abcd_012-");
                Ok(http_client::Response::builder().status(200).body(http_client::AsyncBody::from("<svg xmlns='http://www.w3.org/2000/svg'><text>configured service</text></svg>"))?)
            });
        });
        visual
            .update(cx, |item, cx| {
                item.request_asset(31, "Abcd_012-".into(), true, cx)
            })
            .expect("configured request");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message|matches!(message,HostMessage::Resource{request:31,error:None,bytes,..} if STANDARD.decode(bytes).expect("SVG bytes").windows(18).any(|text|text==b"configured service"))));
        visual.read_with(cx, |item, cx| {
            item.source
                .read(cx)
                .project()
                .expect("project")
                .read(cx)
                .client()
                .http_client()
                .as_fake()
                .replace_handler(|_, _| async move {
                    Ok(http_client::Response::builder()
                        .status(503)
                        .body(http_client::AsyncBody::empty())?)
                })
        });
        visual
            .update(cx, |item, cx| {
                item.request_asset(32, "Abcd_012-".into(), true, cx)
            })
            .expect("service failure request");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message|matches!(message,HostMessage::Resource{request:32,error:Some(error),..} if error.contains("503"))));
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
    }

    #[gpui::test]
    async fn untitled_import_waits_for_save_as_and_cancel_leaves_source_unchanged(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(path!("/project"), serde_json::json!({"docs":{}}))
            .await;
        let project =
            project::Project::test(filesystem.clone(), [path!("/project").as_ref()], cx).await;
        let buffer = project.update(cx, |project, cx| {
            project.create_local_buffer("Untitled", None, true, cx)
        });
        let (multi, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
        let visual = workspace.update_in(cx, |workspace, window, cx| {
            let source = cx.new(|cx| {
                editor::Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
            });
            let visual = cx.new(|cx| MarkdownEditor::new(source, buffer.clone(), window, cx));
            workspace.add_item_to_active_pane(Box::new(visual.clone()), None, true, window, cx);
            visual
        });
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("attach");
        for cancel in [true, false] {
            visual
                .update(cx, |item, cx| {
                    item.import_images(
                        item.protocol.revision,
                        8,
                        ImageInput::Encoded(vec![ImportedImage {
                            name: "image".into(),
                            bytes: STANDARD.encode(png()),
                        }]),
                        cx,
                    )
                })
                .expect("import untitled");
            let mut acknowledged = std::collections::HashSet::new();
            for _ in 0..4 {
                cx.run_until_parked();
                let requests = messages
                    .borrow()
                    .iter()
                    .filter_map(|message| {
                        if let HostMessage::Flush { request } = message {
                            Some(*request)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for request in requests {
                    if acknowledged.insert(request) {
                        visual
                            .update(cx, |item, cx| {
                                item.receive(
                                    protocol::ClientMessage::Flushed {
                                        session: item.protocol.session.clone(),
                                        document: item.protocol.document,
                                        request,
                                        sequence: 0,
                                    },
                                    cx,
                                )
                            })
                            .expect("save flush");
                    }
                }
            }
            assert!(
                !cx.did_prompt_for_paths(),
                "Image picker must follow Save As"
            );
            cx.simulate_new_path_selection(|_| {
                if cancel {
                    None
                } else {
                    Some(PathBuf::from(path!("/project/docs/saved.md")))
                }
            });
            for _ in 0..4 {
                cx.run_until_parked();
                let requests = messages
                    .borrow()
                    .iter()
                    .filter_map(|message| {
                        if let HostMessage::Flush { request } = message {
                            Some(*request)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for request in requests {
                    if acknowledged.insert(request) {
                        visual
                            .update(cx, |item, cx| {
                                item.receive(
                                    protocol::ClientMessage::Flushed {
                                        session: item.protocol.session.clone(),
                                        document: item.protocol.document,
                                        request,
                                        sequence: 0,
                                    },
                                    cx,
                                )
                            })
                            .expect("save as flush");
                    }
                }
            }
            if cancel {
                assert_eq!(
                    buffer.read_with(cx, |buffer, _| buffer.snapshot().text()),
                    "Untitled"
                );
                assert!(buffer.read_with(cx, |buffer, _| buffer.file().is_none()));
            } else {
                assert!(buffer.read_with(cx, |buffer, _| {
                    buffer
                        .snapshot()
                        .text()
                        .starts_with("Untitled\n![image](assets/")
                }));
                assert!(visual.read_with(cx, |item, _| item.error().is_none()));
            }
        }
    }
}
