//! HTTP task execution: segmented ranges, resume state, checksums.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::write_all_at;
use aether_plugin_sdk::task::{SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Budget, Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, LogLevel, Metadata, PluginError,
    PluginEvent, PluginSender, TaskPaths, TaskSpec,
};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, RANGE, RETRY_AFTER};
use thiserror::Error;

use crate::client;
use crate::gate::{parse_retry_after, Cooldown, HostGate};
use crate::metalink;
use crate::state::{self, ResumeState};
use crate::throttle::Throttle;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Detail is for the drawer's schema rows, not the progress bar: 1 Hz is plenty.
const DETAIL_INTERVAL: Duration = Duration::from_secs(1);
const STATE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_RETRIES: u32 = 3;
const CHUNK: usize = 64 * 1024;
/// One Range request covers at most this many bytes. A request is the unit
/// that holds a per-host connection slot, so a bounded request is what lets a
/// shrunken fair share (or a 418 cooldown) take effect within seconds instead
/// of after a whole multi-hundred-MiB range. 16 MiB is also the chunk size the
/// planned work-queue sharding will use, so the request shape stays compatible.
const REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// Give up after this many 418/429 cooldowns; each halves the connection count
/// and backs off exponentially (1, 2, 4 … 30 s), so this bounds wasted time
/// while still letting an anti-leech source settle at one connection.
const MAX_RATE_LIMIT_STRIKES: u32 = 6;

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
        None,
        Arc::new(HostGate::default()),
        task,
        paths,
        Arc::new(Mutex::new(budget)),
        control,
        progress,
    );
}

/// Run one task with a shared budget; `SetBudget` can change the limit while
/// the download is running (aria2 `changeOption`). `fallback` is the
/// auto-negotiation client used when the HTTP/1.1 client cannot talk to an
/// h2-only origin; `gate` is the process-wide per-host connection limiter.
#[allow(clippy::too_many_arguments)]
pub fn run_with_budget(
    sender: PluginSender,
    client: Client,
    fallback: Option<Client>,
    gate: Arc<HostGate>,
    task: TaskSpec,
    paths: TaskPaths,
    budget: Arc<Mutex<Budget>>,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    let result = run_inner(
        &sender,
        &client,
        fallback.as_ref(),
        &gate,
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

#[allow(clippy::too_many_arguments)]
fn run_inner(
    sender: &PluginSender,
    client: &Client,
    fallback: Option<&Client>,
    gate: &Arc<HostGate>,
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
    // The daemon folds a per-task `max_speed_bps` into this budget (including
    // the fair share of the global limit). Only fall back to the raw setting
    // when the caller passed no budget at all.
    let speed_limit_bps = {
        let mut current = budget.lock().unwrap();
        if current.download_bps == 0 {
            if let Some(limit) = task
                .settings
                .get("max_speed_bps")
                .and_then(serde_json::Value::as_u64)
            {
                current.download_bps = limit;
            }
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
    // The resume state must key on the stable source (the task/metalink URL),
    // not the mirror selected for this run.
    let resume_url = resolved_task.url.clone();
    // aria2 addUri accepts extra URIs as mirrors for the same file. Probe
    // every candidate once: the first healthy one becomes the starting source,
    // the rest are kept for mid-download failover when ETag/size match.
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
    let mut probes = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.clone(),
                client::probe_with(client, candidate, &extra),
            )
        })
        .collect::<Vec<_>>();

    // h2-only origins reject an HTTP/1.1 client at the protocol level (ALPN /
    // connection reset), which surfaces as an unusable probe. Only in that
    // case is the multiplexed client used, and the whole candidate list is
    // re-probed so mirror eligibility stays consistent with the real client.
    let mut download_client = client.clone();
    let primary_unusable = probes
        .first()
        .is_some_and(|(_, info)| probe_needs_h2_fallback(info));
    if let Some(fallback) = fallback {
        if primary_unusable {
            let alt = candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.clone(),
                        client::probe_with(fallback, candidate, &extra),
                    )
                })
                .collect::<Vec<_>>();
            if alt.iter().any(|(_, info)| usable(info)) {
                probes = alt;
                download_client = fallback.clone();
                let _ = sender.send(&PluginEvent::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "source rejected HTTP/1.1 ({}); falling back to the h2 client, shards will share one TCP",
                        probes
                            .first()
                            .and_then(|(_, info)| info.reason.clone())
                            .unwrap_or_else(|| "protocol mismatch".into())
                    ),
                    fields: Default::default(),
                });
            }
        }
    }

    let mut active_url = candidates.first().cloned().unwrap_or_default();
    let mut info = probes
        .first()
        .map(|(_, info)| info.clone())
        .unwrap_or_else(|| client::probe_with(&download_client, &active_url, &extra));
    if probes.len() > 1 && (!info.supported || info.reason.is_some()) {
        for (candidate, mirror) in &probes[1..] {
            if mirror.supported && mirror.reason.is_none() {
                active_url = candidate.clone();
                info = mirror.clone();
                break;
            }
        }
    }
    resolved_task.url = active_url.clone();
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

    // Only same-file mirrors are eligible: Range support, identical size and
    // an identical strong ETag. Weak/absent ETags cannot prove the two sources
    // are the same bytes, so mid-download failover stays disabled.
    let has_strong_etag = info
        .etag
        .as_deref()
        .is_some_and(|etag| !etag.trim_start().starts_with("W/"));
    let mut mirrors = vec![active_url.clone()];
    if has_strong_etag {
        for (candidate, mirror) in &probes {
            if candidate == &active_url {
                continue;
            }
            if mirror.supported
                && mirror.reason.is_none()
                && mirror.accept_ranges
                && mirror.size == total
                && mirror.etag == info.etag
            {
                mirrors.push(candidate.clone());
            }
        }
    }

    let mut resume = load_or_create_state(
        paths,
        &resume_url,
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
        .truncate(false)
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
            resume_url.clone(),
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
    let mut mirror_index = 0_usize;
    let mut final_error: Option<DownloadError> = None;
    // Shared by all workers and kept across mirror attempts so a source that
    // taught us to use fewer connections does not get the full count back.
    let cooldown = Arc::new(Cooldown::new(range_count));
    let host_cap = budget.lock().unwrap().per_host_connections.clamp(1, 16);

    // One attempt per compatible source. Completed ranges stay in `shared`,
    // so a failover re-requests only the missing bytes.
    loop {
        let attempt_url = mirrors[mirror_index].clone();
        let host = host_of(&attempt_url);
        // Held for the whole attempt; dropping it releases the host share on
        // completion, failure and cancellation alike.
        let _registration = gate.register(&host, &task.task_id, host_cap);
        let stop = Arc::new(AtomicBool::new(false));
        let (result_tx, result_rx) = mpsc::channel();
        let mut workers = Vec::new();
        for index in 0..range_count as usize {
            let client = download_client.clone();
            let url = attempt_url.clone();
            let host = host.clone();
            let task_id = task.task_id.clone();
            let sender = sender.clone();
            let file = file.clone();
            let shared = shared.clone();
            let downloaded = downloaded.clone();
            let control = control.clone();
            let result_tx = result_tx.clone();
            let extra = extra.clone();
            let throttle = throttle.clone();
            let budget = budget.clone();
            let gate = gate.clone();
            let cooldown = cooldown.clone();
            let stop = stop.clone();
            workers.push(thread::spawn(move || {
                let result = download_range(
                    &client,
                    &url,
                    &host,
                    &task_id,
                    &sender,
                    &file,
                    index,
                    &shared,
                    &downloaded,
                    &control,
                    supports_range,
                    &extra,
                    &throttle,
                    &budget,
                    &gate,
                    &cooldown,
                    &stop,
                );
                let _ = result_tx.send(result);
            }));
        }
        drop(result_tx);

        let mut active = range_count;
        let mut first_error: Option<DownloadError> = None;
        let mut last_bytes = downloaded.load(Ordering::Relaxed);
        let mut last_speed_time = Instant::now();
        let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
        let mut last_detail = Instant::now() - DETAIL_INTERVAL;
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
                        // Stop the remaining workers without touching the
                        // user's cancel/pause state: a mirror failover needs
                        // the same TaskControl for the next attempt.
                        stop.store(true, Ordering::Relaxed);
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

                let eta_secs = eta(bytes, total, speed);
                let _ = sender.send(&PluginEvent::Progress {
                    task_id: task.task_id.clone(),
                    seq: snapshot.seq,
                    downloaded: bytes,
                    total,
                    speed_bps: speed,
                    eta_secs,
                    connections: if control.is_paused() { 0 } else { range_count },
                    chunks_done: Some(done),
                    chunks_total: Some(total_ranges),
                });

                if now.duration_since(last_detail) >= DETAIL_INTERVAL {
                    last_detail = now;
                    let size = match total {
                        Some(total) => format!("{} / {}", human_size(bytes), human_size(total)),
                        None => human_size(bytes),
                    };
                    let _ = sender.send(&PluginEvent::Detail {
                        task_id: task.task_id.clone(),
                        data: serde_json::json!({
                            "speed": format!("{}/s", human_size(speed)),
                            "eta": eta_secs.map(|secs| format!("{secs} s")).unwrap_or_else(|| "-".into()),
                            "size": size,
                        }),
                    });
                }
            }

            if now.duration_since(last_state) >= STATE_INTERVAL {
                persist_state(paths, &shared, sender);
                last_state = now;
            }

            thread::sleep(Duration::from_millis(50));
        }

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

        let Some(error) = first_error else {
            if control.is_canceled() && !is_complete(&shared, total) {
                final_error = Some(DownloadError::Canceled);
            }
            break;
        };

        if matches!(error, DownloadError::Canceled) {
            final_error = Some(error);
            break;
        }

        if mirror_retryable(&error) && mirror_index + 1 < mirrors.len() && !control.is_canceled() {
            mirror_index += 1;
            let next = &mirrors[mirror_index];
            let _ = sender.send(&PluginEvent::Log {
                level: aether_plugin_sdk::LogLevel::Warn,
                message: format!(
                    "source failed ({error}); switching to mirror {next} and keeping completed ranges"
                ),
                fields: Default::default(),
            });
            continue;
        }

        final_error = Some(error);
        break;
    }

    control.cancel();
    if let Some(DownloadError::InvalidRange) = final_error {
        // 416 means the server's view of the file changed; the saved ranges
        // are unusable, so drop them before the daemon retries from scratch.
        let _ = state::remove(&paths.staging);
        let _ = fs::remove_file(&file_path);
    }
    if let Some(error) = final_error {
        return Err(error);
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
    host: &str,
    task_id: &str,
    sender: &PluginSender,
    file: &File,
    index: usize,
    shared: &Arc<Mutex<ResumeState>>,
    downloaded: &Arc<AtomicU64>,
    control: &Arc<TaskControl>,
    supports_range: bool,
    extra: &client::HeaderMap,
    throttle: &Arc<Mutex<Throttle>>,
    budget: &Arc<Mutex<Budget>>,
    gate: &Arc<HostGate>,
    cooldown: &Arc<Cooldown>,
    stop: &Arc<AtomicBool>,
) -> Result<(), DownloadError> {
    let mut attempts = 0;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(DownloadError::Aborted);
        }
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

        // A 418/429 parks every worker of the task until the shared deadline;
        // by then the connection limit has already been halved.
        wait_out_cooldown(control, stop, cooldown)?;
        let _slot = take_slot(gate, host, task_id, cooldown, control, stop)?;

        // One request covers at most `REQUEST_BYTES`, so a slot — and therefore
        // a TCP connection — is never held for a whole multi-hundred-MiB range.
        let request_end = if supports_range {
            end.min(start + done + REQUEST_BYTES - 1)
        } else {
            end
        };
        let mut request =
            client::apply_headers(client.get(url), extra).header(ACCEPT_ENCODING, "identity");
        if supports_range {
            request = request.header(RANGE, format!("bytes={}-{}", start + done, request_end));
        } else if done > 0 {
            request = request.header(RANGE, format!("bytes={done}-"));
        }

        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                drop(_slot);
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
            // 418/429 mean "too many connections", not "bad request": cool the
            // task down and retry with fewer connections instead of failing
            // the whole task in the first round.
            if matches!(status.as_u16(), 418 | 429) {
                let retry_after = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after);
                let (limit, wait) = cooldown.note_rejection(retry_after);
                let _ = sender.send(&PluginEvent::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "HTTP {status} from {host}: cooling down {wait:?} and lowering the task to {limit} connection(s)"
                    ),
                    fields: Default::default(),
                });
                drop(response);
                drop(_slot);
                if cooldown.strikes() >= MAX_RATE_LIMIT_STRIKES {
                    return Err(DownloadError::HttpStatus(status.as_u16()));
                }
                continue;
            }
            drop(_slot);
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
                if stop.load(Ordering::Relaxed) {
                    return Err(DownloadError::Aborted);
                }
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
            Ok(()) => {
                if supports_range {
                    let next = shared.lock().unwrap().ranges[index].next_offset();
                    if next <= request_end {
                        // The body ended before the bytes this request asked
                        // for: a truncated response. Retry the same slice; a
                        // clean slice boundary continues to the next slice.
                        attempts += 1;
                        if attempts >= MAX_RETRIES {
                            return Err(DownloadError::Read(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "response ended before the requested range",
                            )));
                        }
                        backoff(attempts);
                        continue;
                    }
                    if next <= end {
                        continue;
                    }
                }
                return Ok(());
            }
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

fn wait_out_cooldown(
    control: &Arc<TaskControl>,
    stop: &Arc<AtomicBool>,
    cooldown: &Arc<Cooldown>,
) -> Result<(), DownloadError> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(DownloadError::Aborted);
        }
        if !control.wait_until_running() {
            return Err(DownloadError::Canceled);
        }
        let remaining = cooldown.remaining();
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(200)));
    }
}

/// Poll for a per-host slot, staying responsive to pause/cancel and to other
/// tasks on the same host entering or leaving.
fn take_slot(
    gate: &Arc<HostGate>,
    host: &str,
    task_id: &str,
    cooldown: &Arc<Cooldown>,
    control: &Arc<TaskControl>,
    stop: &Arc<AtomicBool>,
) -> Result<crate::gate::Slot, DownloadError> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(DownloadError::Aborted);
        }
        if !control.wait_until_running() {
            return Err(DownloadError::Canceled);
        }
        if let Some(slot) = gate.acquire(host, task_id, cooldown.limit()) {
            return Ok(slot);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Host key for the per-host limiter: the hostname, so different ports and
/// schemes on the same origin still share one cap.
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| url.to_string())
}

fn usable(info: &client::ProbeInfo) -> bool {
    info.supported && info.reason.is_none()
}

/// An HTTP/1.1 probe that failed at the connection/protocol level is the
/// signal to retry with the h2 client. A plain HTTP error status is a source
/// answer, not a protocol mismatch, so it must not trigger the fallback.
fn probe_needs_h2_fallback(info: &client::ProbeInfo) -> bool {
    if usable(info) {
        return false;
    }
    let reason = info.reason.as_deref().unwrap_or_default();
    reason.starts_with("probe failed") || !info.supported
}

/// Errors that justify trying the same byte ranges on another mirror. Disk
/// errors, checksum/state failures and cancellation are not mirror problems.
fn mirror_retryable(error: &DownloadError) -> bool {
    matches!(
        error,
        DownloadError::Http(_)
            | DownloadError::Read(_)
            | DownloadError::HttpStatus(_)
            | DownloadError::RangeIgnored
            | DownloadError::Protocol(_)
    )
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

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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

    #[error("download worker stopped")]
    Aborted,

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
            Self::Aborted => PluginError::new(ErrorKind::Internal, "download worker stopped"),
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
    fn human_size_switches_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(25 * 1024 * 1024), "25.0 MB");
    }

    #[test]
    fn eta_is_none_without_speed() {
        assert_eq!(eta(10, Some(100), 0), None);
        assert_eq!(eta(10, Some(100), 10), Some(9));
    }

    fn probe(reason: Option<&str>, supported: bool) -> client::ProbeInfo {
        client::ProbeInfo {
            supported,
            protocol: Some("https".into()),
            http_version: None,
            final_url: "https://example.test/file".into(),
            size: None,
            etag: None,
            content_type: None,
            filename: None,
            accept_ranges: false,
            reason: reason.map(ToOwned::to_owned),
        }
    }

    /// An h2-only origin fails the HTTP/1.1 probe at the transport level; a
    /// plain status answer must never send the task back to the h2 client.
    #[test]
    fn only_protocol_level_probe_failures_fall_back_to_h2() {
        assert!(probe_needs_h2_fallback(&probe(
            Some("probe failed: error sending request: connection closed"),
            true
        )));
        assert!(probe_needs_h2_fallback(&probe(Some("unsupported"), false)));
        assert!(!probe_needs_h2_fallback(&probe(
            Some("HTTP 403 Forbidden"),
            true
        )));
        assert!(!probe_needs_h2_fallback(&probe(None, true)));
    }

    #[test]
    fn host_key_ignores_port_and_path() {
        assert_eq!(host_of("http://127.0.0.1:8080/a?b=1"), "127.0.0.1");
        assert_eq!(host_of("https://example.test/x"), "example.test");
        assert_eq!(host_of("not a url"), "not a url");
    }
}
