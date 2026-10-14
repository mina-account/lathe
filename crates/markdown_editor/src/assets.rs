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
const MAX_IMAGE_COUNT: usize = 16;
const MAX_IMAGE_BATCH_BYTES: usize = 8 * 1024 * 1024;
type LoadedImage = Result<(String, Vec<u8>)>;
pub(crate) enum ImageInput {
    Encoded(Vec<ImportedImage>),
    Picker,
    Clipboard,
    Paths(Vec<PathBuf>),
}

enum ImageSource {
    Encoded(ImportedImage),
    Clipboard(gpui::Image),
    Path(PathBuf),
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
    if format == image::ImageFormat::Png {
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))?;
        decoder.set_limits(limits)?;
        if decoder.is_apng()? {
            validate_frames(decoder.apng()?.into_frames())?;
        }
    } else if format == image::ImageFormat::Gif {
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
        ImageInput::Encoded(images) => cx.background_spawn(async move {
            check_image_count(images.len())?;
            collect_images(images.into_iter().map(ImageSource::Encoded), &filesystem).await
        }),
        ImageInput::Clipboard => {
            let entries = cx
                .read_from_clipboard()
                .map(|item| item.entries)
                .unwrap_or_default();
            let preflight: Result<()> = (|| {
                let mut count = 0usize;
                let mut raw_bytes = 0usize;
                for entry in &entries {
                    count = count
                        .checked_add(match entry {
                            ClipboardEntry::Image(image) => {
                                raw_bytes = raw_bytes
                                    .checked_add(image.bytes().len())
                                    .context("Clipboard image size overflow")?;
                                anyhow::ensure!(
                                    raw_bytes <= 64 * 1024 * 1024,
                                    "Clipboard image batch exceeds the 64 MiB raw limit"
                                );
                                1
                            }
                            ClipboardEntry::ExternalPaths(paths) => paths.paths().len(),
                            _ => 0,
                        })
                        .context("Image count overflow")?;
                    check_image_count(count)?;
                }
                Ok(())
            })();
            if let Err(error) = preflight {
                return Task::ready(Err(error));
            }
            let mut sources = Vec::new();
            for entry in entries {
                match entry {
                    ClipboardEntry::Image(image) => sources.push(ImageSource::Clipboard(image)),
                    ClipboardEntry::ExternalPaths(paths) => {
                        sources.extend(paths.0.into_iter().map(ImageSource::Path))
                    }
                    _ => {}
                }
            }
            cx.background_spawn(async move { collect_images(sources, &filesystem).await })
        }
        ImageInput::Paths(paths) => cx.background_spawn(async move {
            check_image_count(paths.len())?;
            collect_images(paths.into_iter().map(ImageSource::Path), &filesystem).await
        }),
        ImageInput::Picker => {
            let prompt = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("Select images".into()),
            });
            cx.background_spawn(async move {
                if let Some(paths) = prompt.await?? {
                    check_image_count(paths.len())?;
                    collect_images(paths.into_iter().map(ImageSource::Path), &filesystem).await
                } else {
                    Ok(Vec::new())
                }
            })
        }
    }
}

fn check_image_count(count: usize) -> Result<()> {
    anyhow::ensure!(
        count <= MAX_IMAGE_COUNT,
        "Import at most 16 images in one batch"
    );
    Ok(())
}

async fn collect_images(
    sources: impl IntoIterator<Item = ImageSource>,
    filesystem: &std::sync::Arc<dyn project::Fs>,
) -> Result<Vec<LoadedImage>> {
    let mut images = Vec::new();
    let mut total_bytes = 0usize;
    for source in sources {
        let remaining = MAX_IMAGE_BATCH_BYTES - total_bytes;
        let image = match source {
            ImageSource::Path(path) => load_image_path(filesystem, path, remaining).await?,
            ImageSource::Clipboard(image) => {
                let bytes = image.bytes();
                if image::guess_format(bytes).ok() != Some(image::ImageFormat::Bmp)
                    && bytes.len() <= MAX_IMAGE_BYTES
                {
                    anyhow::ensure!(
                        bytes.len() <= remaining,
                        "Image batch exceeds the 8 MiB encoded limit"
                    );
                }
                clipboard_image(bytes).map(|bytes| ("clipboard".into(), bytes))
            }
            ImageSource::Encoded(image) => STANDARD
                .decode(image.bytes)
                .map(|bytes| (image.name, bytes))
                .map_err(Into::into),
        };
        let image = image.and_then(|(name, bytes)| {
            anyhow::ensure!(
                bytes.len() <= MAX_IMAGE_BYTES,
                "Image exceeds the 4 MiB encoded limit"
            );
            Ok((name, bytes))
        });
        if let Ok((_, bytes)) = &image {
            anyhow::ensure!(
                bytes.len() <= remaining,
                "Image batch exceeds the 8 MiB encoded limit"
            );
            total_bytes += bytes.len();
        }
        images.push(image);
    }
    Ok(images)
}

async fn load_image_path(
    filesystem: &std::sync::Arc<dyn project::Fs>,
    path: PathBuf,
    remaining: usize,
) -> Result<LoadedImage> {
    let reader = match filesystem.open_sync(&path).await {
        Ok(reader) => reader,
        Err(error) => return Ok(Err(error)),
    };
    let mut bytes = Vec::new();
    if let Err(error) = std::io::Read::take(reader, MAX_IMAGE_BYTES.min(remaining) as u64 + 1)
        .read_to_end(&mut bytes)
    {
        return Ok(Err(error.into()));
    }
    anyhow::ensure!(
        bytes.len() <= remaining,
        "Image batch exceeds the 8 MiB encoded limit"
    );
    if bytes.len() > MAX_IMAGE_BYTES {
        return Ok(Err(anyhow::anyhow!(
            "Image exceeds the 4 MiB encoded limit"
        )));
    }
    Ok(Ok((
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into()),
        bytes,
    )))
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
        let case_sensitive = match worktree.read(cx) {
            project::Worktree::Local(local) => local.fs().is_path_case_sensitive(local.abs_path()),
            project::Worktree::Remote(_) => worktree.read(cx).path_style().is_posix(),
        };
        let path_style = worktree.read(cx).path_style();
        let mut supplied = decoded.split('/').peekable();
        for (index, component) in root.trim_end_matches('/').split('/').enumerate() {
            let candidate = supplied
                .next()
                .context("Absolute image is outside the owning worktree")?;
            // Drive letters and UNC addresses retain their casing rules when directory entries are sensitive.
            let windows_prefix = path_style.is_windows()
                && (index == 0 && component.ends_with(':') || root.starts_with("//") && index < 4);
            anyhow::ensure!(
                if case_sensitive && !windows_prefix {
                    candidate == component
                } else {
                    candidate.to_lowercase() == component.to_lowercase()
                },
                "Absolute image is outside the owning worktree"
            );
        }
        anyhow::ensure!(
            supplied.peek().is_some(),
            "Absolute image is outside the owning worktree"
        );
        normalize_path("", &supplied.collect::<Vec<_>>().join("/"))?
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

    struct PumpSurface {
        incoming: Rc<RefCell<Vec<protocol::ClientMessage>>>,
        sent: Rc<RefCell<Vec<HostMessage>>>,
        session: String,
        document: u64,
        sequence: u64,
    }

    impl BrowserSurface for PumpSurface {
        fn send(&mut self, message: &HostMessage) -> Result<()> {
            self.sent.borrow_mut().push(message.clone());
            if let HostMessage::Accepted { sequence, .. } = message {
                self.sequence = *sequence;
            }
            if let HostMessage::Flush { request } = message {
                self.incoming
                    .borrow_mut()
                    .push(protocol::ClientMessage::Flushed {
                        session: self.session.clone(),
                        document: self.document,
                        request: *request,
                        sequence: self.sequence,
                    });
            }
            Ok(())
        }
        fn poll(&mut self) -> Result<Vec<protocol::ClientMessage>> {
            Ok(std::mem::take(&mut *self.incoming.borrow_mut()))
        }
        fn script(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn attach_pump(
        visual: &gpui::Entity<MarkdownEditor>,
        cx: &mut VisualTestContext,
    ) -> (
        Rc<RefCell<Vec<protocol::ClientMessage>>>,
        Rc<RefCell<Vec<HostMessage>>>,
    ) {
        let incoming = Rc::new(RefCell::new(Vec::new()));
        let sent = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(
                    Box::new(PumpSurface {
                        incoming: incoming.clone(),
                        sent: sent.clone(),
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        sequence: 0,
                    }),
                    cx,
                )
            })
            .expect("pump surface");
        (incoming, sent)
    }

    fn pick(item: &MarkdownEditor, clipboard: bool, drop: bool) -> protocol::ClientMessage {
        protocol::ClientMessage::PickImages {
            session: item.protocol.session.clone(),
            document: item.protocol.document,
            base_revision: item.protocol.revision,
            position: 8,
            clipboard,
            drop,
        }
    }

    #[gpui::test(iterations = 20)]
    async fn overlapping_image_requests_and_revision_races_keep_the_pump_editable(
        cx: &mut TestAppContext,
    ) {
        use workspace::item::Item as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        cx.update(|_, cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{"ensure_final_newline_on_save":false,"format_on_save":"off"}"#,
                        cx,
                    )
                    .result()
                    .expect("source-preserving save settings");
            })
        });
        let (incoming, sent) = attach_pump(&visual, cx);
        filesystem.set_create_file_delay(std::time::Duration::from_secs(1));
        incoming.borrow_mut().push(visual.read_with(cx, |item, _| {
            protocol::ClientMessage::ImportImages {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                base_revision: item.protocol.revision,
                position: 8,
                images: vec![ImportedImage {
                    name: "first.png".into(),
                    bytes: STANDARD.encode(png()),
                }],
            }
        }));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        cx.run_until_parked();
        assert_eq!(
            filesystem.pending_file_creations(),
            1,
            "first import awaits its actual file write"
        );
        let second = visual.read_with(cx, |item, _| pick(item, false, false));
        incoming.borrow_mut().push(second);
        visual.update(cx, |item, cx| item.pump_browser(cx));
        assert!(
            visual.read_with(cx, |item, cx| item.asset_active(cx)),
            "ordinary overlap must not retire the pump"
        );
        incoming.borrow_mut().push(visual.read_with(cx, |item, _| {
            protocol::ClientMessage::ImportImages {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                base_revision: item.protocol.revision,
                position: 8,
                images: vec![ImportedImage {
                    name: "overlap.png".into(),
                    bytes: STANDARD.encode(png()),
                }],
            }
        }));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        assert!(
            visual.read_with(cx, |item, cx| item.asset_active(cx)),
            "encoded overlap remains recoverable"
        );
        assert!(
            sent.borrow().iter().any(
                |message| matches!(message,HostMessage::Notice{text} if text.contains("import"))
            )
        );
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| {
                buffer.finalize_last_transaction();
                buffer.edit([(0..0, "Native prefix\n")], None, cx);
                buffer.finalize_last_transaction();
            })
        });
        let stale = visual.read_with(cx, |item, _| protocol::ClientMessage::ImportImages {
            session: item.protocol.session.clone(),
            document: item.protocol.document,
            base_revision: 0,
            position: 8,
            images: vec![ImportedImage {
                name: "stale.png".into(),
                bytes: STANDARD.encode(png()),
            }],
        });
        incoming.borrow_mut().push(stale);
        visual.update(cx, |item, cx| item.pump_browser(cx));
        assert!(
            visual.read_with(cx, |item, cx| item.asset_active(cx)),
            "revision race must remain recoverable"
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        let imported = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        assert!(
            imported.starts_with("Native prefix\nBefore\n\n![image](assets/first-"),
            "{imported}"
        );
        assert_eq!(imported.matches("![image]").count(), 1);
        assert!(imported.ends_with("Tail"));
        let stale = visual.read_with(cx, |item, _| pick(item, true, false));
        let stale_encoded = visual.read_with(cx, |item, _| protocol::ClientMessage::ImportImages {
            session: item.protocol.session.clone(),
            document: item.protocol.document,
            base_revision: item.protocol.revision,
            position: 8,
            images: vec![ImportedImage {
                name: "stale-after.png".into(),
                bytes: STANDARD.encode(png()),
            }],
        });
        visual.update(cx, |item, cx| {
            item.buffer
                .update(cx, |buffer, cx| buffer.edit([(0..0, "Race\n")], None, cx))
        });
        incoming.borrow_mut().push(stale);
        incoming.borrow_mut().push(stale_encoded);
        visual.update(cx, |item, cx| item.pump_browser(cx));
        assert!(visual.read_with(cx, |item, cx| item.asset_active(cx)));
        let edit = visual.read_with(cx, |item, cx| {
            let length = item
                .buffer
                .read(cx)
                .snapshot()
                .text()
                .encode_utf16()
                .count();
            protocol::ClientMessage::Edit {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                request: 1,
                sequence: 1,
                base_revision: item.protocol.revision,
                change: protocol::Change {
                    start: length,
                    end: length,
                    text: " edited".into(),
                },
            }
        });
        incoming.borrow_mut().push(visual.read_with(cx, |item, cx| {
            protocol::ClientMessage::Draft {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                sequence: 1,
                base_revision: item.protocol.revision,
                text: item.buffer.read(cx).snapshot().text() + " edited",
            }
        }));
        incoming.borrow_mut().push(edit);
        visual.update(cx, |item, cx| item.pump_browser(cx));
        let edited = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        assert!(edited.ends_with("Tail edited"));
        let project = visual.read_with(cx, |item, cx| {
            item.source.read(cx).project().expect("project").clone()
        });
        let save = visual.update_in(cx, |item, window, cx| {
            item.save(Default::default(), project, window, cx)
        });
        visual.update(cx, |item, cx| item.pump_browser(cx));
        save.await.expect("save after recoverable import rejection");
        assert_eq!(
            filesystem
                .read_file_sync(path!("/project/docs/doc.md"))
                .expect("saved source"),
            edited.as_bytes()
        );
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
        });
        assert!(
            !visual
                .read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text())
                .ends_with(" edited")
        );
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| buffer.redo(cx))
        });
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            edited
        );
        let close = visual.update_in(cx, |item, window, cx| item.prepare_to_save(window, cx));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        close
            .await
            .expect("close preparation after recoverable import rejection");
    }

    #[gpui::test]
    async fn native_image_count_limits_reject_before_opening_picker_clipboard_and_drop_paths(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let (incoming, _) = attach_pump(&visual, cx);
        for route in ["picker", "clipboard", "drop"] {
            let paths = (0..17)
                .map(|index| PathBuf::from(path!("/project")).join(format!("{route}-{index}.png")))
                .collect::<Vec<_>>();
            for path in &paths {
                filesystem.write(path, &png()).await.expect("input fixture");
            }
            if route == "clipboard" {
                cx.update(|_, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem {
                        entries: vec![ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                            paths.clone().into(),
                        ))],
                    })
                });
            }
            if route == "drop" {
                visual.update(cx, |item, cx| {
                    item.native_image_drop(&gpui::ExternalPaths(paths.clone().into()), cx)
                });
            }
            incoming.borrow_mut().push(visual.read_with(cx, |item, _| {
                pick(item, route == "clipboard", route == "drop")
            }));
            visual.update(cx, |item, cx| item.pump_browser(cx));
            cx.run_until_parked();
            if route == "picker" {
                cx.simulate_path_prompt_response(|_| Some(paths.clone()));
                cx.run_until_parked();
            }
            assert!(
                visual.read_with(cx, |item, _| item
                    .error()
                    .is_some_and(|error| error.contains("16 images"))),
                "{route}: visible count limit"
            );
            assert_eq!(
                paths
                    .iter()
                    .map(|path| filesystem.open_count_for_path(path))
                    .sum::<usize>(),
                0,
                "{route}: rejected before opening files"
            );
            assert_eq!(
                visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
                "Before\n\nTail"
            );
            assert!(visual.read_with(cx, |item, cx| item.asset_active(cx)));
        }
    }

    #[gpui::test]
    async fn native_image_aggregate_limits_stop_reads_with_bounded_collection(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let (incoming, _) = attach_pump(&visual, cx);
        for route in ["picker", "clipboard", "drop"] {
            let paths = (0..4)
                .map(|index| {
                    PathBuf::from(path!("/project")).join(format!("{route}-large-{index}.png"))
                })
                .collect::<Vec<_>>();
            let mut bytes = png();
            bytes.resize(MAX_IMAGE_BYTES, 0);
            for path in &paths {
                filesystem.write(path, &bytes).await.expect("input fixture");
            }
            if route == "clipboard" {
                cx.update(|_, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem {
                        entries: vec![ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                            paths.clone().into(),
                        ))],
                    })
                });
            }
            if route == "drop" {
                visual.update(cx, |item, cx| {
                    item.native_image_drop(&gpui::ExternalPaths(paths.clone().into()), cx)
                });
            }
            incoming.borrow_mut().push(visual.read_with(cx, |item, _| {
                pick(item, route == "clipboard", route == "drop")
            }));
            visual.update(cx, |item, cx| item.pump_browser(cx));
            cx.run_until_parked();
            if route == "picker" {
                cx.simulate_path_prompt_response(|_| Some(paths.clone()));
                cx.run_until_parked();
            }
            assert!(
                visual.read_with(cx, |item, _| item
                    .error()
                    .is_some_and(|error| error.contains("8 MiB"))),
                "{route}: visible aggregate limit"
            );
            assert!(
                paths
                    .iter()
                    .map(|path| filesystem.bytes_read_for_path(path))
                    .sum::<usize>()
                    <= 8 * 1024 * 1024 + 1,
                "{route}: bounded reads including one overflow sentinel"
            );
            assert_eq!(
                filesystem.open_count_for_path(paths.last().expect("last path")),
                0,
                "{route}: stops before remaining selections"
            );
            assert_eq!(
                visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
                "Before\n\nTail"
            );
            assert!(visual.read_with(cx, |item, cx| item.asset_active(cx)));
        }
    }

    #[gpui::test]
    async fn native_clipboard_image_and_mixed_batches_obey_limits_and_accept_the_exact_boundary(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let (incoming, _) = attach_pump(&visual, cx);
        let small = gpui::Image::from_bytes(gpui::ImageFormat::Png, png());
        let path = PathBuf::from(path!("/project/clipboard-count.png"));
        filesystem
            .write(&path, &png())
            .await
            .expect("clipboard path");
        for mixed in [false, true] {
            let mut entries = (0..if mixed { 16 } else { 17 })
                .map(|_| ClipboardEntry::Image(small.clone()))
                .collect::<Vec<_>>();
            if mixed {
                entries.push(ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                    vec![path.clone()].into(),
                )));
            }
            cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem { entries }));
            incoming
                .borrow_mut()
                .push(visual.read_with(cx, |item, _| pick(item, true, false)));
            visual.update(cx, |item, cx| item.pump_browser(cx));
            cx.run_until_parked();
            assert!(visual.read_with(cx, |item, _| {
                item.error()
                    .is_some_and(|error| error.contains("16 images"))
            }));
            assert_eq!(filesystem.open_count_for_path(&path), 0);
        }
        let mut bitmap = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bitmap, image::ImageFormat::Bmp)
            .expect("native BMP fixture");
        let mut excessive_raw = bitmap.get_ref().clone();
        excessive_raw.resize(32 * 1024 * 1024 + 1, 0);
        let excessive_raw = gpui::Image::from_bytes(gpui::ImageFormat::Bmp, excessive_raw);
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem {
                entries: vec![ClipboardEntry::Image(excessive_raw); 2],
            })
        });
        incoming
            .borrow_mut()
            .push(visual.read_with(cx, |item, _| pick(item, true, false)));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        cx.run_until_parked();
        assert!(visual.read_with(cx, |item, _| {
            item.error()
                .is_some_and(|error| error.contains("64 MiB raw"))
        }));
        let mut bytes = png();
        bytes.resize(MAX_IMAGE_BYTES, 0);
        let large = gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes);
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem {
                entries: vec![ClipboardEntry::Image(large.clone()); 3],
            })
        });
        incoming
            .borrow_mut()
            .push(visual.read_with(cx, |item, _| pick(item, true, false)));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        cx.run_until_parked();
        assert!(visual.read_with(cx, |item, _| {
            item.error().is_some_and(|error| error.contains("8 MiB"))
        }));
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem {
                entries: vec![ClipboardEntry::Image(large); 2],
            })
        });
        incoming
            .borrow_mut()
            .push(visual.read_with(cx, |item, _| pick(item, true, false)));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        cx.run_until_parked();
        assert_eq!(
            visual.read_with(cx, |item, cx| item
                .buffer
                .read(cx)
                .snapshot()
                .text()
                .matches("![image]")
                .count()),
            2
        );
        assert!(visual.read_with(cx, |item, cx| item.asset_active(cx)));
        let before = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        let animation = apng(2, 2, 2);
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem {
                entries: vec![
                    ClipboardEntry::Image(gpui::Image::from_bytes(
                        gpui::ImageFormat::Bmp,
                        bitmap.into_inner(),
                    )),
                    ClipboardEntry::Image(gpui::Image::from_bytes(
                        gpui::ImageFormat::Png,
                        animation.clone(),
                    )),
                ],
            })
        });
        incoming
            .borrow_mut()
            .push(visual.read_with(cx, |item, _| pick(item, true, false)));
        visual.update(cx, |item, cx| item.pump_browser(cx));
        cx.run_until_parked();
        let after = visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text());
        assert_eq!(after.matches("![image]").count(), 4);
        assert!(
            filesystem
                .files_with_contents(path!("/project/docs/assets").as_ref())
                .iter()
                .any(|(_, bytes)| *bytes == animation)
        );
        for _ in 0..2 {
            visual.update(cx, |item, cx| {
                item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
            });
        }
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            before
        );
    }

    fn apng(width: u32, height: u32, frames: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            encoder.set_animated(frames, 0).expect("animation header");
            let mut writer = encoder.write_header().expect("PNG header");
            for index in 0..frames {
                let pixels = vec![index as u8; width as usize * height as usize * 4];
                writer.write_image_data(&pixels).expect("animation frame");
            }
            writer.finish().expect("complete APNG");
        }
        bytes
    }

    #[test]
    fn apng_frame_limit_rejects_a_valid_101_frame_animation() {
        use image::AnimationDecoder as _;
        let accepted = apng(1, 1, 100);
        assert_eq!(
            image_type(&accepted).expect("100-frame PNG"),
            ("png", "image/png")
        );
        let excessive = apng(1, 1, 101);
        let frames = image::codecs::png::PngDecoder::new(Cursor::new(&excessive))
            .expect("PNG")
            .apng()
            .expect("APNG")
            .into_frames()
            .collect_frames()
            .expect("valid animation");
        assert_eq!(frames.len(), 101);
        assert!(
            image_type(&excessive).is_err(),
            "APNG must obey the 100-frame cap"
        );
    }

    #[test]
    fn apng_total_decoded_limit_rejects_a_small_encoded_animation() {
        let accepted = apng(1024, 1024, 16);
        assert!(accepted.len() < MAX_IMAGE_BYTES);
        image_type(&accepted).expect("64 MiB animation boundary");
        let excessive = apng(1024, 1024, 17);
        assert!(excessive.len() < MAX_IMAGE_BYTES);
        assert!(
            image_type(&excessive).is_err(),
            "APNG must obey the cumulative 64 MiB cap"
        );
    }

    #[gpui::test]
    async fn apng_import_and_resource_validation_surface_errors_without_broken_links(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        let messages = Rc::new(RefCell::new(Vec::new()));
        visual
            .update(cx, |item, cx| {
                item.attach(Box::new(Recording(messages.clone())), cx)
            })
            .expect("recording surface");
        let excessive = apng(1, 1, 101);
        filesystem
            .write(
                path!("/project/docs/assets/excessive.png").as_ref(),
                &excessive,
            )
            .await
            .expect("harmless APNG fixture");
        visual
            .update(cx, |item, cx| {
                item.request_asset(90, "assets/excessive.png".into(), false, cx)
            })
            .expect("resource request");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message| matches!(message, HostMessage::Resource{request:90,bytes,error:Some(error),..} if bytes.is_empty() && error.contains("100 decoded frames"))));
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    0,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "excessive.png".into(),
                        bytes: STANDARD.encode(excessive),
                    }]),
                    cx,
                )
            })
            .expect("import request");
        cx.run_until_parked();
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
        assert!(visual.read_with(cx, |item, _| {
            item.error()
                .is_some_and(|error| error.contains("100 decoded frames"))
        }));
        assert_eq!(
            filesystem
                .read_dir(path!("/project/docs/assets").as_ref())
                .await
                .expect("asset directory")
                .collect::<Vec<_>>()
                .await
                .len(),
            2
        );
        let accepted = apng(2, 2, 2);
        filesystem
            .write(
                path!("/project/docs/assets/accepted.png").as_ref(),
                &accepted,
            )
            .await
            .expect("bounded APNG");
        visual
            .update(cx, |item, cx| {
                item.request_asset(91, "assets/accepted.png".into(), false, cx)
            })
            .expect("bounded resource");
        cx.run_until_parked();
        assert!(messages.borrow().iter().any(|message|matches!(message,HostMessage::Resource{request:91,mime,bytes,error:None} if mime=="image/png" && STANDARD.decode(bytes).expect("response bytes")==accepted)));
        visual
            .update(cx, |item, cx| {
                item.import_images(
                    0,
                    0,
                    ImageInput::Encoded(vec![ImportedImage {
                        name: "accepted.png".into(),
                        bytes: STANDARD.encode(accepted),
                    }]),
                    cx,
                )
            })
            .expect("bounded import");
        cx.run_until_parked();
        assert!(visual.read_with(cx, |item, cx| {
            item.buffer
                .read(cx)
                .snapshot()
                .text()
                .starts_with("![image](assets/accepted-")
        }));
        visual.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| buffer.undo(cx))
        });
        assert_eq!(
            visual.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text()),
            "Before\n\nTail"
        );
    }

    #[gpui::test]
    async fn absolute_image_roots_follow_the_local_filesystem_sensitivity(cx: &mut TestAppContext) {
        let (visual, filesystem, cx) = fixture(cx).await;
        let (project, document, root) = visual.read_with(cx, |item, cx| {
            let document =
                ProjectPath::from_file(item.buffer.read(cx).file().expect("file").as_ref(), cx);
            let project = item.source.read(cx).project().expect("project").clone();
            let root = project
                .read(cx)
                .worktree_for_id(document.worktree_id, cx)
                .expect("root")
                .read(cx)
                .abs_path()
                .to_string_lossy()
                .replace('\\', "/");
            (project, document, root)
        });
        let supplied = format!("{}/docs/assets/same.png", root.to_uppercase());
        assert!(
            cx.read(|cx| image_resource_path(&document, &supplied, &project, cx))
                .is_err()
        );
        filesystem.set_case_sensitive(false);
        let path = cx
            .read(|cx| image_resource_path(&document, &supplied, &project, cx))
            .expect("case-insensitive owning filesystem");
        assert_eq!(path.path.as_unix_str(), "docs/assets/same.png");
        assert!(
            cx.read(|cx| image_resource_path(
                &document,
                &format!("{root}-sibling/image.png"),
                &project,
                cx
            ))
            .is_err()
        );
    }

    #[cfg(target_os = "windows")]
    #[gpui::test]
    async fn windows_absolute_image_roots_follow_the_owning_filesystem_case_rules(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs as _;
        let (visual, filesystem, cx) = fixture(cx).await;
        filesystem.set_case_sensitive(false);
        let project = visual.read_with(cx, |item, cx| {
            item.source.read(cx).project().expect("project").clone()
        });
        for root in [
            "C:/MixedProject",
            "//Server/Share/MixedProject",
            "C:/Prójéct",
            "//Sérver/Share/Prójéct",
        ] {
            filesystem
                .insert_tree(root, serde_json::json!({"doc.md":"Image", "assets":{}}))
                .await;
            filesystem
                .write(&std::path::Path::new(root).join("assets/image.png"), &png())
                .await
                .expect("case fixture");
            let (worktree, _) = project
                .update(cx, |project, cx| {
                    project.find_or_create_worktree(root, true, cx)
                })
                .await
                .expect("Windows root");
            let document = ProjectPath {
                worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
                path: rel_path("doc.md").into(),
            };
            for supplied in [
                format!("{root}/assets/image.png"),
                format!("{}/assets/image.png", root.to_lowercase()),
                format!(
                    "{}\\assets\\image.png",
                    root.to_uppercase().replace('/', "\\")
                ),
            ] {
                let path = cx
                    .read(|cx| image_resource_path(&document, &supplied, &project, cx))
                    .expect("in-root Windows case variant");
                assert_eq!(path.path.as_unix_str(), "assets/image.png");
                assert_eq!(
                    project
                        .update(cx, |project, cx| project.load_binary_resource(
                            path,
                            MAX_IMAGE_BYTES,
                            cx
                        ))
                        .await
                        .expect("canonical bytes"),
                    png()
                );
            }
            for supplied in [
                format!("{root}-sibling/assets/image.png"),
                format!("{root}/../outside.png"),
            ] {
                assert!(
                    cx.read(|cx| image_resource_path(&document, &supplied, &project, cx))
                        .is_err(),
                    "outside or sibling root"
                );
            }
            filesystem
                .insert_tree("C:/outside-case", serde_json::json!({}))
                .await;
            filesystem
                .write(std::path::Path::new("C:/outside-case/image.png"), &png())
                .await
                .expect("outside fixture");
            filesystem
                .insert_symlink(
                    std::path::Path::new(root).join("assets/escape.png"),
                    std::path::PathBuf::from("C:/outside-case/image.png"),
                )
                .await;
            let supplied = format!("{}/assets/escape.png", root.to_lowercase());
            let path = cx
                .read(|cx| image_resource_path(&document, &supplied, &project, cx))
                .expect("lexically in-root alias");
            assert!(
                project
                    .update(cx, |project, cx| project.load_binary_resource(
                        path,
                        MAX_IMAGE_BYTES,
                        cx
                    ))
                    .await
                    .is_err(),
                "canonical escape remains denied"
            );
            filesystem.set_case_sensitive(true);
            let prefix_variant = if root.starts_with("C:") {
                root.replacen("C:", "c:", 1)
            } else {
                root.replace("Server", "SERVER")
                    .replace("Sérver", "SÉRVER")
                    .replace("Share", "SHARE")
            };
            assert!(
                cx.read(|cx| image_resource_path(
                    &document,
                    &format!("{prefix_variant}/assets/image.png"),
                    &project,
                    cx
                ))
                .is_ok(),
                "drive or UNC address case remains independent of directory case"
            );
            assert!(
                cx.read(|cx| image_resource_path(
                    &document,
                    &format!("{}/assets/image.png", root.to_lowercase()),
                    &project,
                    cx
                ))
                .is_err(),
                "case-sensitive owning filesystem"
            );
            filesystem.set_case_sensitive(false);
        }
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
