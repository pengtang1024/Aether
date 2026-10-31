//! HTTP task execution: segmented ranges, resume state, checksums.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::write_all_at;
use aether_plugin_sdk::task::{SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Budget, Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, Metadata, PluginError, PluginEvent,
    PluginSender, TaskPaths, TaskSpec,
};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, RANGE};
use thiserror::Error;

use crate::client;
use crate::metalink;
use crate::state::{self, ResumeState};
use crate::throttle::Throttle;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const STATE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_RETRIES: u32 = 3;
const CHUNK: usize = 64 * 1024;

/// Run one task with a private budget. Kept for plugins that reuse the HTTP
/// downloader (cloud drives, IPFS gateways, …) and do not need live updates.
pub fn run(
    sender: PluginSender,
    client: Client,
    task: TaskSpec,
    paths: TaskPaths,
    budget: Budget,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    run_with_budget(
        sender,
        client,
        task,
        paths,
        Arc::new(Mutex::new(budget)),
        control,
        progress,
    );
}

/// Run one task with a shared budget; `SetBudget` can change the limit while
/// the download is running (aria2 `changeOption`).
pub fn run_with_budget(
    sender: PluginSender,
    client: Client,
    task: TaskSpec,
    paths: TaskPaths,
    budget: Arc<Mutex<Budget>>,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    let result = run_inner(
        &sender,
        &client,
        &task,
        &paths,
        budget,
        control.clone(),
        &progress,
    );

    match result {
        Ok(()) => {}
        Err(DownloadError::Canceled) => {}
        Err(error) => {
            let plugin_error = error.to_plugin_error();
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: plugin_error,
            });
        }
    }
}

fn run_inner(
    sender: &PluginSender,
    client: &Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    budget: Arc<Mutex<Budget>>,
    control: Arc<TaskControl>,
    progress: &SharedProgress,
) -> Result<(), DownloadError> {
    if control.is_canceled() {
        return Err(DownloadError::Canceled);
    }

    let mut resolved_task = task.clone();
    let extra = client::headers_from_settings(&task.settings)
        .map_err(|error| DownloadError::Protocol(error.to_string()))?;
    // The daemon already folds a per-task `max_speed_bps` into this budget;
    // keep the old fallback for direct/contract callers.
    let speed_limit_bps = {
        let mut current = budget.lock().unwrap();
        if let Some(limit) = task
            .settings
            .get("max_speed_bps")
            .and_then(serde_json::Value::as_u64)
        {
            current.download_bps = limit;
        }
        current.download_bps
    };
    let throttle = Arc::new(Mutex::new(Throttle::new(speed_limit_bps, Instant::now())));

    if let Some(info) = metalink::resolve_with(client, &task.url, &extra)
        .map_err(|error| DownloadError::Protocol(error.to_string()))?
    {
        resolved_task.url = info.url;
        resolved_task.filename = Some(info.filename);
        resolved_task.expected_size = info.size.or(resolved_task.expected_size);
        if resolved_task.checksum.is_none() {
            resolved_task.checksum = info.checksum;
        }
    }
    // aria2 addUri accepts extra URIs as mirrors for the same file. Probe the
    // primary first and fall back to a mirror only when it looks unhealthy;
    // mid-download failover is not wired yet.
    let mut candidates = vec![resolved_task.url.clone()];
    if let Some(mirrors) = resolved_task
        .settings
        .get("mirrors")
        .and_then(serde_json::Value::as_array)
    {
        candidates.extend(
            mirrors
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|url| !url.is_empty())
                .map(str::to_string),
        );
    }
    let primary = candidates.first().cloned().unwrap_or_default();
    let mut info = client::probe_with(client, &primary, &extra);
    if candidates.len() > 1 && (!info.supported || info.reason.is_some()) {
        for candidate in &candidates[1..] {
            let mirror = client::probe_with(client, candidate, &extra);
            if mirror.supported && mirror.reason.is_none() {
                resolved_task.url = candidate.clone();
                info = mirror;
                break;
            }
        }
    }
    let task = &resolved_task;

    fs::create_dir_all(&paths.staging)?;

    if !info.supported {
        return Err(DownloadError::Protocol(
            info.reason.unwrap_or_else(|| "unsupported URL".into()),
        ));
    }

    let filename = task
        .filename
        .as_deref()
        .and_then(aether_plugin_sdk::fsutil::sanitize_filename)
        .or_else(|| info.filename.clone())
        .or_else(|| client::filename_from_url(&reqwest::Url::parse(&task.url).ok()?))
        .unwrap_or_else(|| "download.bin".into());

    let total = task.expected_size.or(info.size).filter(|size| *size > 0);
    let supports_range = info.accept_ranges && total.is_some();
    let connections = if supports_range {
        budget.lock().unwrap().connections.clamp(1, 16)
    } else {
        1
    };

    let mut resume = load_or_create_state(
        paths,
        &task.url,
        info.etag.as_deref(),
        total,
        &filename,
        supports_range,
        connections,
    )?;

    if control.is_canceled() {
        return Err(DownloadError::Canceled);
    }

    let file_path = paths.staging.join(&filename);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&file_path)?;

    if let Some(total) = total {
        if file.metadata()?.len() != total {
            file.set_len(total)?;
        }
    }

    if resume.ranges.is_empty() {
        resume = ResumeState::new(
            task.url.clone(),
            info.etag.clone(),
            total.unwrap_or(0),
            filename.clone(),
            supports_range,
            connections,
        );
    }

    let total_downloaded = resume.downloaded();
    let shared = Arc::new(Mutex::new(resume));
    let downloaded = Arc::new(AtomicU64::new(total_downloaded));
    let range_count = shared.lock().unwrap().ranges.len() as u32;

    {
        let mut snapshot = progress.lock().unwrap();
        snapshot.downloaded = total_downloaded;
        snapshot.total = total;
        snapshot.connections = range_count;
        snapshot.chunks_total = range_count;
    }

    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: info.final_url.clone(),
            size: total,
            etag: info.etag.clone(),
            content_type: info.content_type.clone(),
            files: vec![],
        },
    });

    let file = Arc::new(file);
    let (result_tx, result_rx) = mpsc::channel();
    let mut workers = Vec::new();
    for index in 0..range_count as usize {
        let client = client.clone();
        let url = task.url.clone();
        let file = file.clone();
        let shared = shared.clone();
        let downloaded = downloaded.clone();
        let control = control.clone();
        let result_tx = result_tx.clone();
        let extra = extra.clone();
        let throttle = throttle.clone();
        let budget = budget.clone();
        workers.push(thread::spawn(move || {
            let result = download_range(
                &client,
                &url,
                &file,
                index,
                &shared,
                &downloaded,
                &control,
                supports_range,
                &extra,
                &throttle,
                &budget,
            );
            let _ = result_tx.send(result);
        }));
    }
    drop(result_tx);

    let mut active = range_count;
    let mut first_error: Option<DownloadError> = None;
    let mut last_bytes = total_downloaded;
    let mut last_speed_time = Instant::now();
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut last_state = Instant::now();
    let mut disconnected = false;

    loop {
        if control.is_canceled() {
            break;
        }

        loop {
            match result_rx.try_recv() {
                Ok(Ok(())) => active = active.saturating_sub(1),
                Ok(Err(error)) => {
                    active = active.saturating_sub(1);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    // Stop the remaining workers as soon as one fails.
                    if let Some(error) = &first_error {
                        if !matches!(error, DownloadError::Canceled) {
                            control.cancel();
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if disconnected || active == 0 {
            break;
        }

        let now = Instant::now();
        if now.duration_since(last_progress) >= PROGRESS_INTERVAL {
            let bytes = downloaded.load(Ordering::Relaxed);
            let elapsed = now.duration_since(last_speed_time).as_secs_f64();
            let speed = if elapsed > 0.0 && !control.is_paused() {
                ((bytes.saturating_sub(last_bytes)) as f64 / elapsed) as u64
            } else {
                0
            };
            last_bytes = bytes;
            last_speed_time = now;
            last_progress = now;

            let (done, total_ranges) = {
                let state = shared.lock().unwrap();
                (
                    state
                        .ranges
                        .iter()
                        .filter(|range| range.is_complete())
                        .count() as u32,
                    state.ranges.len() as u32,
                )
            };

            let mut snapshot = progress.lock().unwrap();
            snapshot.downloaded = bytes;
            snapshot.total = total;
            snapshot.speed_bps = speed;
            snapshot.chunks_done = done;
            snapshot.chunks_total = total_ranges;
            snapshot.seq = snapshot.seq.saturating_add(1);

            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq: snapshot.seq,
                downloaded: bytes,
                total,
                speed_bps: speed,
                eta_secs: eta(bytes, total, speed),
                connections: if control.is_paused() { 0 } else { range_count },
                chunks_done: Some(done),
                chunks_total: Some(total_ranges),
            });
        }

        if now.duration_since(last_state) >= STATE_INTERVAL {
            persist_state(paths, &shared, sender);
            last_state = now;
        }

        thread::sleep(Duration::from_millis(50));
    }

    control.cancel();
    for worker in workers {
        if worker.join().is_err() && first_error.is_none() {
            first_error = Some(DownloadError::Protocol("download worker panicked".into()));
        }
    }
    if active > 0 && first_error.is_none() {
        first_error = Some(DownloadError::Protocol(
            "download worker stopped before the range completed".into(),
        ));
    }
    persist_state(paths, &shared, sender);

    if let Some(DownloadError::InvalidRange) = first_error {
        // 416 means the server's view of the file changed; the saved ranges
        // are unusable, so drop them before the daemon retries from scratch.
        let _ = state::remove(&paths.staging);
        let _ = fs::remove_file(&file_path);
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if control.is_canceled() && first_error.is_none() && !is_complete(&shared, total) {
        return Err(DownloadError::Canceled);
    }

    let final_size = file.metadata()?.len();
    if let Some(total) = total {
        if final_size != total {
            return Err(DownloadError::Protocol(format!(
                "size mismatch: expected {total}, got {final_size}"
            )));
        }
    }

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(DownloadError::Protocol(
                "only SHA-256 checksums are supported".into(),
            ));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(&file_path);
            let _ = state::remove(&paths.staging);
            return Err(DownloadError::ChecksumMismatch);
        }
    }

    state::remove(&paths.staging)?;
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![FileInfo {
            relative_path: filename,
            size: final_size,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: Some(serde_json::json!({
            "etag": info.etag,
            "final_url": info.final_url,
        })),
    })?;

    Ok(())
}

fn load_or_create_state(
    paths: &TaskPaths,
    url: &str,
    etag: Option<&str>,
    total: Option<u64>,
    filename: &str,
    supports_range: bool,
    connections: u32,
) -> Result<ResumeState, DownloadError> {
    if let Some(state) = state::read(&paths.staging)? {
        if state.matches(url, etag, total)
            && state.filename == filename
            && state.supports_range == supports_range
        {
            return Ok(state);
        }
        state::remove(&paths.staging)?;
    }

    Ok(ResumeState::new(
        url,
        etag.map(ToOwned::to_owned),
        total.unwrap_or(0),
        filename,
        supports_range,
        connections,
    ))
}

fn persist_state(paths: &TaskPaths, shared: &Arc<Mutex<ResumeState>>, sender: &PluginSender) {
    let snapshot = shared.lock().unwrap().clone();
    if let Err(error) = state::write(&paths.staging, &snapshot) {
        let _ = sender.send(&PluginEvent::Log {
            level: aether_plugin_sdk::LogLevel::Warn,
            message: format!("failed to persist resume state: {error}"),
            fields: Default::default(),
        });
    }
}

fn is_complete(shared: &Arc<Mutex<ResumeState>>, total: Option<u64>) -> bool {
    if total.is_none() {
        return true;
    }
    shared.lock().unwrap().is_complete()
}

#[allow(clippy::too_many_arguments)]
fn download_range(
    client: &Client,
    url: &str,
    file: &File,
    index: usize,
    shared: &Arc<Mutex<ResumeState>>,
    downloaded: &Arc<AtomicU64>,
    control: &Arc<TaskControl>,
    supports_range: bool,
    extra: &client::HeaderMap,
    throttle: &Arc<Mutex<Throttle>>,
    budget: &Arc<Mutex<Budget>>,
) -> Result<(), DownloadError> {
    let mut attempts = 0;

    loop {
        if !control.wait_until_running() {
            return Err(DownloadError::Canceled);
        }

        let (start, end, done) = {
            let state = shared.lock().unwrap();
            let range = state.ranges[index];
            (range.start, range.end, range.done)
        };

        if supports_range && start + done > end {
            return Ok(());
        }

        let mut request =
            client::apply_headers(client.get(url), extra).header(ACCEPT_ENCODING, "identity");
        if supports_range {
            request = request.header(RANGE, format!("bytes={}-{}", start + done, end));
        } else if done > 0 {
            request = request.header(RANGE, format!("bytes={done}-"));
        }

        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                if attempts < MAX_RETRIES && retryable_reqwest(&error) {
                    attempts += 1;
                    backoff(attempts);
                    continue;
                }
                return Err(DownloadError::Http(error));
            }
        };

        let status = response.status();
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(DownloadError::InvalidRange);
        }
        if !status.is_success() {
            if attempts < MAX_RETRIES && retryable_status(status.as_u16()) {
                attempts += 1;
                backoff(attempts);
                continue;
            }
            return Err(DownloadError::HttpStatus(status.as_u16()));
        }

        if supports_range && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(DownloadError::RangeIgnored);
        }

        let mut offset = if supports_range { start + done } else { done };
        if !supports_range && status == reqwest::StatusCode::OK && done > 0 {
            // Server ignored the resume Range. Only one worker exists in this
            // mode, so restarting the file is safe.
            file.set_len(0)?;
            downloaded.fetch_sub(done, Ordering::Relaxed);
            let mut state = shared.lock().unwrap();
            state.ranges[index].done = 0;
            offset = 0;
        }

        let mut response = response;
        let mut buffer = vec![0_u8; CHUNK];
        let read_result = (|| -> Result<(), DownloadError> {
            loop {
                if !control.wait_until_running() {
                    return Err(DownloadError::Canceled);
                }
                let read = match response.read(&mut buffer) {
                    Ok(read) => read,
                    // A mid-stream reset/timeout is resumable; a disk write
                    // failure below is not, so the two paths stay distinct.
                    Err(error) => return Err(DownloadError::Read(error)),
                };
                if read == 0 {
                    break;
                }
                // Shared token bucket: the limit applies to the whole task,
                // and `SetBudget` (aria2 changeOption) updates it live.
                let limit = budget.lock().unwrap().download_bps;
                let delay = {
                    let mut throttle = throttle.lock().unwrap();
                    throttle.set_limit(limit);
                    throttle.delay_for(read, Instant::now())
                };
                if !delay.is_zero() {
                    // Waiting while paused costs nothing extra; the bucket
                    // refills only as real time passes.
                    thread::sleep(delay);
                }
                write_all_at(file, &buffer[..read], offset)?;
                offset += read as u64;
                downloaded.fetch_add(read as u64, Ordering::Relaxed);
                let mut state = shared.lock().unwrap();
                state.ranges[index].done += read as u64;
                if !supports_range {
                    state.ranges[index].end = offset.saturating_sub(1);
                }
            }
            Ok(())
        })();

        match read_result {
            Ok(()) => return Ok(()),
            Err(DownloadError::Canceled) => return Err(DownloadError::Canceled),
            Err(error) => {
                if attempts < MAX_RETRIES && retryable_error(&error) {
                    attempts += 1;
                    backoff(attempts);
                    continue;
                }
                return Err(error);
            }
        }
    }
}

fn retryable_error(error: &DownloadError) -> bool {
    match error {
        DownloadError::Http(error) => retryable_reqwest(error),
        DownloadError::HttpStatus(status) => retryable_status(*status),
        DownloadError::Read(error) => retryable_read(error),
        _ => false,
    }
}

fn retryable_read(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            // reqwest's blocking read timeout is surfaced as `Other`.
            | io::ErrorKind::Other
    )
}

fn retryable_reqwest(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request()
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500..=599)
}

fn backoff(attempt: u32) {
    thread::sleep(Duration::from_millis(200 * 2_u64.pow(attempt.min(4))));
}

fn eta(downloaded: u64, total: Option<u64>, speed: u64) -> Option<u64> {
    let total = total?;
    if speed == 0 {
        return None;
    }
    Some(total.saturating_sub(downloaded) / speed)
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("network read error: {0}")]
    Read(io::Error),

    #[error("http status {0}")]
    HttpStatus(u16),

    #[error("server ignored a Range request")]
    RangeIgnored,

    #[error("range is no longer satisfiable")]
    InvalidRange,

    #[error("canceled")]
    Canceled,

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("state error: {0}")]
    State(#[from] state::StateError),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl DownloadError {
    /// reqwest's `Display` stops at the request URL and hides the underlying
    /// cause (DNS failure, TLS handshake, connection refused, …). The message
    /// is all the user ever sees in the task log, so walk the source chain
    /// into it.
    fn reqwest_message(error: &reqwest::Error) -> String {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        message
    }

    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::ChecksumMismatch => {
                PluginError::new(ErrorKind::Checksum, "SHA-256 mismatch").retryable(true)
            }
            Self::HttpStatus(status) => {
                let kind = match status {
                    401 | 403 => ErrorKind::Auth,
                    404 | 410 => ErrorKind::HttpStatus,
                    416 => ErrorKind::RangeMismatch,
                    _ => ErrorKind::HttpStatus,
                };
                PluginError::new(kind, format!("HTTP {status}"))
                    .retryable(retryable_status(*status))
                    .with_status(*status)
            }
            Self::Http(error) if error.is_timeout() => {
                PluginError::new(ErrorKind::Timeout, Self::reqwest_message(error)).retryable(true)
            }
            Self::Http(error) if error.is_connect() => {
                PluginError::new(ErrorKind::Connect, Self::reqwest_message(error)).retryable(true)
            }
            Self::Http(error) => {
                PluginError::new(ErrorKind::Protocol, Self::reqwest_message(error))
            }
            Self::Read(error) => {
                let kind = if retryable_read(error) {
                    ErrorKind::Connect
                } else {
                    ErrorKind::Internal
                };
                PluginError::new(kind, error.to_string()).retryable(retryable_read(error))
            }
            Self::RangeIgnored => PluginError::new(ErrorKind::RangeMismatch, self.to_string()),
            Self::InvalidRange => {
                PluginError::new(ErrorKind::RangeMismatch, self.to_string()).retryable(true)
            }
            Self::State(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Io(error) => {
                let kind = match error.kind() {
                    io::ErrorKind::PermissionDenied => ErrorKind::Permission,
                    io::ErrorKind::StorageFull => ErrorKind::DiskFull,
                    _ => ErrorKind::Internal,
                };
                PluginError::new(kind, error.to_string()).retryable(false)
            }
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_full_is_not_retryable() {
        let error = DownloadError::Io(io::Error::new(
            io::ErrorKind::StorageFull,
            "No space left on device",
        ))
        .to_plugin_error();
        assert_eq!(error.kind, ErrorKind::DiskFull);
        assert!(!error.retryable);

        let denied = DownloadError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            .to_plugin_error();
        assert_eq!(denied.kind, ErrorKind::Permission);
    }

    #[test]
    fn eta_is_none_without_speed() {
        assert_eq!(eta(10, Some(100), 0), None);
        assert_eq!(eta(10, Some(100), 10), Some(9));
    }
}
