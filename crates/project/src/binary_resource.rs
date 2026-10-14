use crate::{Project, ProjectPath};
use anyhow::{Context as _, Result, anyhow, bail};
use client::proto;
use futures::{FutureExt, channel::oneshot, future::Either};
use gpui::{AsyncApp, Context, Entity, Task};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(crate) fn next_file_id() -> u64 {
    static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) struct BinaryResourceDownload {
    path: ProjectPath,
    limit: usize,
    size: Option<usize>,
    bytes: Vec<u8>,
    completion: Option<oneshot::Sender<Result<Vec<u8>>>>,
}

impl Project {
    pub fn load_shared_binary_resource(
        &mut self,
        path: ProjectPath,
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<u8>>> {
        let Some(worktree) = self.worktree_for_id(path.worktree_id, cx) else {
            return Task::ready(Err(anyhow!("Resource worktree is unavailable")));
        };
        let private = match worktree.read(cx) {
            worktree::Worktree::Local(worktree) => worktree.is_path_private(&path.path),
            worktree::Worktree::Remote(_) => worktree
                .read(cx)
                .snapshot()
                .entry_for_path(&path.path)
                .is_some_and(|entry| entry.is_private),
        };
        if private {
            return Task::ready(Err(anyhow!("Private project resources cannot be shared")));
        }
        self.load_binary_resource_with_privacy(path, limit, true, cx)
    }

    pub fn load_binary_resource(
        &mut self,
        path: ProjectPath,
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<u8>>> {
        self.load_binary_resource_with_privacy(path, limit, false, cx)
    }

    fn load_binary_resource_with_privacy(
        &mut self,
        path: ProjectPath,
        limit: usize,
        reject_private: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<u8>>> {
        self.binary_resource_downloads.retain(|_, download| {
            download
                .completion
                .as_ref()
                .is_some_and(|sender| !sender.is_canceled())
        });
        if self.binary_resource_downloads.len() >= 16 {
            return Task::ready(Err(anyhow!("Too many pending project byte resources")));
        }
        if self.is_disconnected(cx) {
            return Task::ready(Err(anyhow!("Project is disconnected")));
        }
        let Some(worktree) = self.worktree_for_id(path.worktree_id, cx) else {
            return Task::ready(Err(anyhow!("Resource worktree is unavailable")));
        };
        if worktree.read(cx).is_local() {
            let load = worktree.update(cx, |worktree, cx| {
                if reject_private {
                    worktree.load_shared_binary_file_with_limit(&path.path, limit, cx)
                } else {
                    worktree.load_binary_file_with_limit(&path.path, limit, cx)
                }
            });
            return cx.spawn(async move |_, _| {
                let content = load.await?.content;
                if content.len() > limit {
                    bail!("Project resource exceeds its byte limit");
                }
                Ok(content)
            });
        }
        let client: client::AnyProtoClient = if let Some(remote_client) = &self.remote_client {
            remote_client.read(cx).proto_client()
        } else {
            self.collab_client.clone().into()
        };
        let project_id = if self.remote_client.is_some() {
            crate::REMOTE_SERVER_PROJECT_ID
        } else {
            self.remote_id().unwrap_or(crate::REMOTE_SERVER_PROJECT_ID)
        };
        let file_id = next_file_id();
        let (completion, receiver) = oneshot::channel();
        self.binary_resource_downloads.insert(
            file_id,
            BinaryResourceDownload {
                path: path.clone(),
                limit,
                size: None,
                bytes: Vec::new(),
                completion: Some(completion),
            },
        );
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(31))
                .await;
            if let Err(error) = this.update(cx, |this, _| {
                this.binary_resource_downloads.remove(&file_id);
            }) {
                log::debug!("Project resource cleanup: {error:#}");
            }
        })
        .detach();
        let timeout = cx.background_executor().timer(Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            let result = async {
                let response = client
                    .request(proto::DownloadFileByPath {
                        project_id,
                        worktree_id: path.worktree_id.to_proto(),
                        path: path.path.as_unix_str().into(),
                        file_id,
                        byte_limit: Some(limit as u64),
                        reject_private,
                    })
                    .await?;
                if response.file_id != file_id {
                    bail!("Resource response identity mismatch");
                }
                receiver
                    .await
                    .map_err(|_| anyhow!("Project resource download cancelled"))?
            }
            .boxed();
            let result = match futures::future::select(result, timeout).await {
                Either::Left((result, _)) => result,
                Either::Right(_) => Err(anyhow!(
                    "Project resource download timed out or disconnected"
                )),
            };
            this.update(cx, |this, _| {
                this.binary_resource_downloads.remove(&file_id);
            })?;
            result
        })
    }

    pub(crate) async fn handle_binary_resource_request(
        this: Entity<Self>,
        envelope: client::TypedEnvelope<proto::DownloadFileByPath>,
        mut cx: AsyncApp,
    ) -> Result<proto::DownloadFileResponse> {
        let peer_id = envelope.original_sender_id()?;
        let request = envelope.payload;
        let limit = request
            .byte_limit
            .context("Shared project resources require a byte limit")?
            .min(4 * 1024 * 1024) as usize;
        let path = ProjectPath {
            worktree_id: crate::WorktreeId::from_proto(request.worktree_id),
            path: util::rel_path::RelPath::from_unix_str(&request.path)?.into(),
        };
        let load = this.update(&mut cx, |this, cx| {
            this.load_shared_binary_resource(path, limit, cx)
        });
        let content = load.await?;
        let client = this.read_with(&cx, |this, _| this.collab_client.clone());
        use proto::create_file_for_peer::Variant;
        client.send(proto::CreateFileForPeer {
            project_id: request.project_id,
            peer_id: Some(peer_id),
            variant: Some(Variant::State(proto::FileState {
                id: request.file_id,
                file: Some(proto::File {
                    worktree_id: request.worktree_id,
                    path: request.path,
                    ..Default::default()
                }),
                content_size: content.len() as u64,
            })),
        })?;
        for chunk in content.chunks(1024 * 1024) {
            client.send(proto::CreateFileForPeer {
                project_id: request.project_id,
                peer_id: Some(peer_id),
                variant: Some(Variant::Chunk(proto::FileChunk {
                    file_id: request.file_id,
                    data: chunk.to_vec(),
                })),
            })?;
        }
        Ok(proto::DownloadFileResponse {
            file_id: request.file_id,
        })
    }

    pub(crate) fn receive_binary_resource(&mut self, message: &proto::CreateFileForPeer) -> bool {
        use proto::create_file_for_peer::Variant;
        let file_id = match &message.variant {
            Some(Variant::State(state)) => state.id,
            Some(Variant::Chunk(chunk)) => chunk.file_id,
            None => return false,
        };
        let Some(download) = self.binary_resource_downloads.get_mut(&file_id) else {
            return false;
        };
        let result = match &message.variant {
            Some(Variant::State(state)) => {
                let valid_path = state.file.as_ref().is_some_and(|file| {
                    file.worktree_id == download.path.worktree_id.to_proto()
                        && file.path == download.path.path.as_unix_str()
                });
                if !valid_path
                    || download.size.is_some()
                    || state.content_size > download.limit as u64
                {
                    Some(Err(anyhow!("Invalid or oversized project resource state")))
                } else {
                    download.size = Some(state.content_size as usize);
                    if state.content_size == 0 {
                        Some(Ok(Vec::new()))
                    } else {
                        None
                    }
                }
            }
            Some(Variant::Chunk(chunk)) => {
                if download.size.is_none()
                    || chunk.data.len() > download.limit.saturating_sub(download.bytes.len())
                {
                    Some(Err(anyhow!("Invalid or oversized project resource chunk")))
                } else {
                    download.bytes.extend_from_slice(&chunk.data);
                    match download.size {
                        Some(size) if download.bytes.len() > size => {
                            Some(Err(anyhow!("Project resource exceeds declared size")))
                        }
                        Some(size) if download.bytes.len() == size => {
                            Some(Ok(std::mem::take(&mut download.bytes)))
                        }
                        _ => None,
                    }
                }
            }
            None => None,
        };
        if let Some(result) = result {
            if let Some(completion) = download.completion.take() {
                if completion.send(result).is_err() {
                    log::debug!("Project resource receiver was closed");
                }
            }
        }
        true
    }
}
