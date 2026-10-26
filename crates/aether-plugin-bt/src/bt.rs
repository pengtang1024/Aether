//! BitTorrent task execution on top of librqbit.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, FileInfo, Metadata, PluginEvent, PluginSender, TaskPaths, TaskSpec,
};
use anyhow::Context;
use librqbit::api::TorrentIdOrHash;
use librqbit::{AddTorrent, AddTorrentOptions, Session, TorrentStatsState};
use thiserror::Error;
use tokio::time::sleep;
use url::Url;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const INIT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct BtProbe {
    pub supported: bool,
    pub filename: Option<String>,
    pub reason: Option<String>,
}

pub fn probe(input: &str) -> BtProbe {
    if input.starts_with("magnet:") {
        let name = Url::parse(input)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "dn")
                    .map(|(_, value)| value.into_owned())
            })
            .and_then(|name| aether_plugin_sdk::fsutil::sanitize_filename(&name));
        return BtProbe {
            supported: true,
            filename: name,
            reason: None,
        };
    }

    if let Ok(url) = Url::parse(input) {
        if matches!(url.scheme(), "http" | "https")
            && url.path().to_ascii_lowercase().ends_with(".torrent")
        {
            let name = url
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .and_then(|name| {
                    aether_plugin_sdk::fsutil::sanitize_filename(name.trim_end_matches(".torrent"))
                });
            return BtProbe {
                supported: true,
                filename: name,
                reason: None,
            };
        }
    }

    BtProbe {
        supported: false,
        filename: None,
        reason: Some("expected a magnet link or an http(s) .torrent URL".into()),
    }
}

pub fn run(
    sender: PluginSender,
    runtime: Arc<tokio::runtime::Runtime>,
    session: Arc<Session>,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    let result = runtime.block_on(run_inner(
        &sender, &session, &task, &paths, &control, &progress,
    ));

    match result {
        Ok(()) => {}
        Err(BtError::Canceled) => {}
        Err(error) => {
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: error.to_plugin_error(),
            });
        }
    }
}

async fn run_inner(
    sender: &PluginSender,
    session: &Arc<Session>,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
) -> Result<(), BtError> {
    if control.is_canceled() {
        return Err(BtError::Canceled);
    }
    std::fs::create_dir_all(&paths.staging)?;

    let mut options = AddTorrentOptions {
        output_folder: Some(paths.staging.to_string_lossy().into_owned()),
        overwrite: true,
        ..Default::default()
    };
    if let Some(peers) = initial_peers(&task.settings) {
        options.initial_peers = Some(peers);
    }

    let response = session
        .add_torrent(AddTorrent::from_url(task.url.clone()), Some(options))
        .await
        .context("add torrent")?;
    let handle = response
        .into_handle()
        .ok_or_else(|| anyhow::anyhow!("torrent was not added"))?;

    let initialized = tokio::time::timeout(INIT_TIMEOUT, handle.wait_until_initialized()).await;
    match initialized {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(BtError::Anyhow(error)),
        Err(_) => return Err(BtError::Timeout),
    }

    let files = handle
        .with_metadata(|metadata| {
            metadata
                .file_infos
                .iter()
                .map(|file| (file.relative_filename.clone(), file.len))
                .collect::<Vec<_>>()
        })
        .context("torrent metadata")?;
    let total = files.iter().map(|(_, len)| *len).sum::<u64>();
    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: task.url.clone(),
            size: Some(total),
            etag: None,
            content_type: Some("application/x-bittorrent".into()),
            files: files
                .iter()
                .map(|(path, len)| FileInfo {
                    relative_path: path.to_string_lossy().replace('\\', "/"),
                    size: *len,
                    checksum: None,
                })
                .collect(),
        },
    });

    let mut snapshot = ProgressSnapshot {
        total: Some(total),
        connections: 0,
        ..Default::default()
    };
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut seq = 0_u64;

    loop {
        if control.is_canceled() {
            let _ = session
                .delete(TorrentIdOrHash::Id(handle.id()), false)
                .await;
            return Err(BtError::Canceled);
        }

        if control.is_paused() {
            let _ = session.pause(&handle).await;
            while control.is_paused() {
                if control.is_canceled() {
                    let _ = session
                        .delete(TorrentIdOrHash::Id(handle.id()), false)
                        .await;
                    return Err(BtError::Canceled);
                }
                sleep(Duration::from_millis(100)).await;
            }
            let _ = session.unpause(&handle).await;
        }

        let stats = handle.stats();
        let speed = stats
            .live
            .as_ref()
            .map(|live| (live.download_speed.mbps * 1024.0 * 1024.0) as u64)
            .unwrap_or(0);
        let downloaded = stats.progress_bytes;
        let total_now = stats.total_bytes.max(total);
        let finished = stats.finished || matches!(stats.state, TorrentStatsState::Error);

        snapshot.downloaded = downloaded;
        snapshot.total = Some(total_now);
        snapshot.speed_bps = speed;
        snapshot.seq = seq;
        snapshot.connections = 0;
        snapshot.chunks_total = 0;
        *progress.lock().unwrap() = snapshot.clone();

        if last_progress.elapsed() >= PROGRESS_INTERVAL || finished {
            seq = seq.saturating_add(1);
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq,
                downloaded,
                total: Some(total_now),
                speed_bps: speed,
                eta_secs: (speed > 0).then(|| total_now.saturating_sub(downloaded) / speed),
                connections: 0,
                chunks_done: None,
                chunks_total: None,
            });
            last_progress = Instant::now();
        }

        if let Some(error) = stats.error {
            return Err(BtError::Protocol(error));
        }
        if finished {
            break;
        }

        sleep(PROGRESS_INTERVAL).await;
    }

    handle
        .wait_until_completed()
        .await
        .context("wait for torrent completion")?;

    let mut completed_files = Vec::new();
    for (relative, size) in &files {
        let path = paths.staging.join(relative);
        let checksum = aether_plugin_sdk::checksum::sha256_file(&path)?;
        completed_files.push(FileInfo {
            relative_path: relative.to_string_lossy().replace('\\', "/"),
            size: *size,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        });
    }

    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: completed_files,
        plugin_state: Some(serde_json::json!({
            "total": total,
            "torrent_handle": handle.id().to_string(),
        })),
    })?;

    Ok(())
}

fn initial_peers(settings: &serde_json::Value) -> Option<Vec<SocketAddr>> {
    let values = settings.get("initial_peers")?.as_array()?;
    let peers = values
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(|value| value.parse().ok())
        .collect::<Vec<_>>();
    (!peers.is_empty()).then_some(peers)
}

#[derive(Debug, Error)]
pub enum BtError {
    #[error("bittorrent error: {0}")]
    Anyhow(#[from] anyhow::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("canceled")]
    Canceled,

    #[error("timed out waiting for torrent metadata")]
    Timeout,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl BtError {
    pub fn to_plugin_error(&self) -> aether_plugin_sdk::PluginError {
        use aether_plugin_sdk::ErrorKind;
        match self {
            Self::Canceled => aether_plugin_sdk::PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::Timeout => aether_plugin_sdk::PluginError::new(
                ErrorKind::Timeout,
                "torrent metadata timed out",
            )
            .retryable(true),
            Self::Anyhow(error) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Protocol, error.to_string())
            }
            Self::Io(error) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Internal, error.to_string())
            }
            Self::Sdk(error) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Internal, error.to_string())
            }
            Self::Protocol(message) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Protocol, message.clone())
            }
        }
    }
}
