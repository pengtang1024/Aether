//! BitTorrent task execution on top of librqbit.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, FileInfo, Metadata, PluginEvent, PluginSender, TaskPaths, TaskSpec,
};
use anyhow::Context;
use librqbit::api::{Api, TorrentIdOrHash};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, TorrentStatsState,
    ValidatedTorrentMetaV1Info,
};
use thiserror::Error;
use tokio::time::sleep;
use url::Url;

use crate::settings::{self, BtSettings, FileSelector, TorrentFile};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const SEED_DETAIL_INTERVAL: Duration = Duration::from_secs(1);
const INIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Live peer list in aria2's object shape (used for `aria2.getPeers` via the
/// daemon's cached `Detail` payload).
pub(crate) fn peer_snapshot(session: &Arc<Session>, id: usize) -> Vec<serde_json::Value> {
    let api = Api::new(session.clone(), None);
    match api.api_peer_stats(TorrentIdOrHash::Id(id), Default::default()) {
        Ok(snapshot) => snapshot
            .peers
            .into_iter()
            .map(|(address, stats)| peer_json(address.as_str(), stats.state))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn peer_json(address: &str, state: &str) -> serde_json::Value {
    let (ip, port) = address.rsplit_once(':').unwrap_or((address, "0"));
    let ip = ip.trim_start_matches('[').trim_end_matches(']');
    serde_json::json!({
        "peerId": "",
        "ip": ip,
        "port": port,
        "bitfield": "",
        "amChoking": "false",
        "peerChoking": "false",
        "downloadSpeed": "0",
        "uploadSpeed": "0",
        "seeder": "false",
        "state": state,
    })
}

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
    let settings = BtSettings::from_value(&task.settings).map_err(BtError::Settings)?;

    if control.is_canceled() {
        return Err(BtError::Canceled);
    }
    std::fs::create_dir_all(&paths.staging)?;

    let mut options = add_options(paths, settings.initial_peers.clone(), false);
    let mut source = AddTorrent::from_url(task.url.clone());
    if let Some(selectors) = &settings.files {
        if selectors
            .iter()
            .any(|selector| matches!(selector, FileSelector::Path(_)))
        {
            // Paths can only be mapped once the metadata is known. Listing the
            // torrent resolves a magnet (or fetches a .torrent) without adding
            // it to the session, and hands back the exact bytes to add next.
            let listed = tokio::time::timeout(
                INIT_TIMEOUT,
                session.add_torrent(
                    AddTorrent::from_url(task.url.clone()),
                    Some(add_options(paths, settings.initial_peers.clone(), true)),
                ),
            )
            .await
            .map_err(|_| BtError::Timeout)?
            .context("list torrent files")?;
            let AddTorrentResponse::ListOnly(listed) = listed else {
                return Err(BtError::Protocol(
                    "torrent listing returned no metadata".into(),
                ));
            };
            let files = files_from_info(&listed.info).context("torrent metadata")?;
            options.only_files =
                Some(settings::resolve_files(&files, selectors).map_err(BtError::Settings)?);
            source = AddTorrent::from_bytes(listed.torrent_bytes);
        } else {
            options.only_files = Some(index_selectors(selectors));
        }
    }

    let response = session
        .add_torrent(source, Some(options))
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
                .map(|file| TorrentFile {
                    path: file.relative_filename.to_string_lossy().replace('\\', "/"),
                    len: file.len,
                })
                .collect::<Vec<_>>()
        })
        .context("torrent metadata")?;
    let selected = match &settings.files {
        Some(selectors) => {
            Some(settings::resolve_files(&files, selectors).map_err(BtError::Settings)?)
        }
        None => None,
    };
    let total = match &selected {
        Some(indices) => indices.iter().map(|index| files[*index].len).sum(),
        None => files.iter().map(|file| file.len).sum(),
    };
    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: task.url.clone(),
            size: Some(total),
            etag: None,
            content_type: Some("application/x-bittorrent".into()),
            files: files
                .iter()
                .map(|file| FileInfo {
                    relative_path: file.path.clone(),
                    size: file.len,
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
    let mut last_peers = Instant::now() - SEED_DETAIL_INTERVAL;
    let mut seq = 0_u64;

    loop {
        if control.is_canceled() {
            let _ = session
                .delete(TorrentIdOrHash::Id(handle.id()), false)
                .await;
            return Err(BtError::Canceled);
        }

        wait_if_paused(session, &handle, control).await?;

        let stats = handle.stats();
        let speed = stats
            .live
            .as_ref()
            .map(|live| (live.download_speed.mbps * 1024.0 * 1024.0) as u64)
            .unwrap_or(0);
        let downloaded = stats.progress_bytes;
        // only_files makes librqbit report the whole torrent in `total_bytes`;
        // the task total must stay the sum of the selected files.
        let total_now = if selected.is_some() {
            total
        } else {
            stats.total_bytes.max(total)
        };
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

        if last_peers.elapsed() >= SEED_DETAIL_INTERVAL {
            let peers = peer_snapshot(session, handle.id());
            let _ = sender.send(&PluginEvent::Detail {
                task_id: task.task_id.clone(),
                data: serde_json::json!({
                    "seeding": false,
                    "peers": peers,
                }),
            });
            last_peers = Instant::now();
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

    let seed_report = if settings.seeds() {
        Some(
            seed_until_limit(
                SeedContext {
                    sender,
                    session,
                    task,
                    handle: &handle,
                    control,
                    progress,
                    total,
                    settings: &settings,
                },
                &mut seq,
            )
            .await?,
        )
    } else {
        None
    };

    if let Some(indices) = &selected {
        remove_unselected_placeholders(paths, &files, indices);
    }

    let mut completed_files = Vec::new();
    for file in completed_files_list(&files, selected.as_deref()) {
        let path = paths.staging.join(&file.path);
        let checksum = aether_plugin_sdk::checksum::sha256_file(&path)?;
        completed_files.push(FileInfo {
            relative_path: file.path.clone(),
            size: file.len,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        });
    }

    let plugin_state = match seed_report {
        Some(report) => serde_json::json!({
            "total": total,
            "torrent_handle": handle.id().to_string(),
            "uploaded_bytes": report.uploaded_bytes,
            "ratio": report.ratio,
            "seeded_seconds": report.seconds,
        }),
        None => serde_json::json!({
            "total": total,
            "torrent_handle": handle.id().to_string(),
        }),
    };

    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: completed_files,
        plugin_state: Some(plugin_state),
    })?;

    Ok(())
}

struct SeedReport {
    uploaded_bytes: u64,
    ratio: f64,
    seconds: u64,
}

struct SeedContext<'a> {
    sender: &'a PluginSender,
    session: &'a Arc<Session>,
    task: &'a TaskSpec,
    handle: &'a Arc<ManagedTorrent>,
    control: &'a TaskControl,
    progress: &'a SharedProgress,
    total: u64,
    settings: &'a BtSettings,
}

/// Keep the torrent seeding until the configured ratio or duration is reached.
///
/// Any positive limit arms this phase; with both configured the first one to
/// be satisfied wins. The torrent is paused when the phase ends so a ratio cap
/// really stops the upload.
async fn seed_until_limit(context: SeedContext<'_>, seq: &mut u64) -> Result<SeedReport, BtError> {
    let SeedContext {
        sender,
        session,
        task,
        handle,
        control,
        progress,
        total,
        settings,
    } = context;
    let started = Instant::now();
    let duration_limit = settings.seed_duration();
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut last_detail = Instant::now() - SEED_DETAIL_INTERVAL;

    let report = loop {
        if control.is_canceled() {
            let _ = session
                .delete(TorrentIdOrHash::Id(handle.id()), false)
                .await;
            return Err(BtError::Canceled);
        }

        wait_if_paused(session, handle, control).await?;

        let stats = handle.stats();
        let uploaded = stats.uploaded_bytes;
        let ratio = if total == 0 {
            0.0
        } else {
            uploaded as f64 / total as f64
        };
        let elapsed = started.elapsed();

        {
            let mut snapshot = progress.lock().unwrap();
            snapshot.downloaded = total;
            snapshot.total = Some(total);
            snapshot.speed_bps = 0;
            snapshot.seq = *seq;
            snapshot.connections = 0;
            snapshot.chunks_total = 0;
        }

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            *seq = seq.saturating_add(1);
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq: *seq,
                downloaded: total,
                total: Some(total),
                speed_bps: 0,
                eta_secs: None,
                connections: 0,
                chunks_done: None,
                chunks_total: None,
            });
            last_progress = Instant::now();
        }

        if last_detail.elapsed() >= SEED_DETAIL_INTERVAL {
            let _ = sender.send(&PluginEvent::Detail {
                task_id: task.task_id.clone(),
                data: serde_json::json!({
                    "seeding": true,
                    "ratio": ratio,
                    "uploaded_bytes": uploaded,
                    "seed_seconds": elapsed.as_secs(),
                    "peers": peer_snapshot(session, handle.id()),
                }),
            });
            last_detail = Instant::now();
        }

        if let Some(error) = stats.error {
            return Err(BtError::Protocol(error));
        }

        let ratio_done = settings.seed_ratio <= 0.0 || total == 0 || ratio >= settings.seed_ratio;
        let time_done = duration_limit.is_none_or(|limit| elapsed >= limit);
        if ratio_done && time_done {
            break SeedReport {
                uploaded_bytes: uploaded,
                ratio,
                seconds: elapsed.as_secs(),
            };
        }

        sleep(PROGRESS_INTERVAL).await;
    };

    let _ = session.pause(handle).await;
    Ok(report)
}

async fn wait_if_paused(
    session: &Arc<Session>,
    handle: &Arc<ManagedTorrent>,
    control: &TaskControl,
) -> Result<(), BtError> {
    if !control.is_paused() {
        return Ok(());
    }

    let _ = session.pause(handle).await;
    while control.is_paused() {
        if control.is_canceled() {
            let _ = session
                .delete(TorrentIdOrHash::Id(handle.id()), false)
                .await;
            return Err(BtError::Canceled);
        }
        sleep(Duration::from_millis(100)).await;
    }
    let _ = session.unpause(handle).await;
    Ok(())
}

fn add_options(
    paths: &TaskPaths,
    initial_peers: Option<Vec<SocketAddr>>,
    list_only: bool,
) -> AddTorrentOptions {
    AddTorrentOptions {
        output_folder: Some(paths.staging.to_string_lossy().into_owned()),
        overwrite: true,
        list_only,
        initial_peers,
        ..Default::default()
    }
}

fn files_from_info<Buf: AsRef<[u8]>>(
    info: &ValidatedTorrentMetaV1Info<Buf>,
) -> anyhow::Result<Vec<TorrentFile>> {
    info.iter_file_details()
        .map(|details| {
            Ok(TorrentFile {
                path: details
                    .filename
                    .to_pathbuf()
                    .to_string_lossy()
                    .replace('\\', "/"),
                len: details.len,
            })
        })
        .collect()
}

fn index_selectors(selectors: &[FileSelector]) -> Vec<usize> {
    let mut indices = selectors
        .iter()
        .filter_map(|selector| match selector {
            FileSelector::Index(index) => Some(*index),
            FileSelector::Path(_) => None,
        })
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn completed_files_list<'a>(
    files: &'a [TorrentFile],
    selected: Option<&[usize]>,
) -> Vec<&'a TorrentFile> {
    match selected {
        Some(indices) => indices.iter().map(|index| &files[*index]).collect(),
        None => files.iter().collect(),
    }
}

/// librqbit creates an empty placeholder for every file in the torrent, even
/// when `only_files` excludes it. Drop those placeholders so the staging dir
/// only contains what the task actually downloaded. Best effort: Windows may
/// still hold the handle, in which case the empty file stays and is simply not
/// reported in `Completed`.
fn remove_unselected_placeholders(paths: &TaskPaths, files: &[TorrentFile], selected: &[usize]) {
    for (index, file) in files.iter().enumerate() {
        if selected.contains(&index) {
            continue;
        }
        let path = paths.staging.join(&file.path);
        let empty = std::fs::metadata(&path)
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(false);
        if empty {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[derive(Debug, Error)]
pub enum BtError {
    #[error("bittorrent error: {0}")]
    Anyhow(#[from] anyhow::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("invalid settings: {0}")]
    Settings(String),

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
            Self::Settings(message) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Protocol, message.clone())
            }
            Self::Protocol(message) => {
                aether_plugin_sdk::PluginError::new(ErrorKind::Protocol, message.clone())
            }
        }
    }
}

#[cfg(test)]
mod peer_tests {
    use super::peer_json;

    #[test]
    fn peer_json_splits_ipv4_and_ipv6_addresses() {
        let v4 = peer_json("1.2.3.4:6881", "live");
        assert_eq!(v4["ip"], "1.2.3.4");
        assert_eq!(v4["port"], "6881");

        let v6 = peer_json("[::1]:6881", "live");
        assert_eq!(v6["ip"], "::1");
        assert_eq!(v6["port"], "6881");
        assert_eq!(v6["state"], "live");
    }
}
