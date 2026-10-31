//! S3 download execution: single stream and concurrent ranged GET.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::{sanitize_filename, write_all_at};
use aether_plugin_sdk::task::{SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Budget, Checksum, ChecksumAlgorithm, FileInfo, Metadata, PluginEvent, PluginSender, TaskPaths,
    TaskSpec,
};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_LENGTH;
use reqwest::StatusCode;
use url::Url;

use crate::s3::{send, Config, S3Error, Target};

const CHUNK: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Objects below this size are downloaded with a single connection.
const MULTIPART_THRESHOLD: u64 = 4 * 1024 * 1024;
const MIN_SEGMENT: u64 = 1024 * 1024;
const MAX_CONNECTIONS: u32 = 8;

pub fn run(
    sender: PluginSender,
    client: Client,
    task: TaskSpec,
    paths: TaskPaths,
    budget: Budget,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    match run_inner(&sender, &client, &task, &paths, budget, &control, &progress) {
        Ok(()) => {}
        Err(S3Error::Canceled) => {}
        Err(error) => {
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: error.to_plugin_error(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    sender: &PluginSender,
    client: &Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    budget: Budget,
    control: &Arc<TaskControl>,
    progress: &SharedProgress,
) -> Result<(), S3Error> {
    if control.is_canceled() {
        return Err(S3Error::Canceled);
    }
    let target = Target::parse(&task.url)?;
    let config = Arc::new(Config::from_settings(&task.settings)?);
    let url = target.url(&config)?;
    fs::create_dir_all(&paths.staging)?;

    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .or_else(|| target.filename())
        .unwrap_or_else(|| "download.bin".into());
    let file_path = paths.staging.join(&filename);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&file_path)?;

    // The staging file length is the contiguous prefix already on disk, so a
    // resumed task only asks for the bytes that are still missing.
    let mut prefix = file.metadata()?.len();
    let total = head_size(client, &url, &config)
        .or(task.expected_size)
        .filter(|size| *size > 0);
    if let Some(total) = total {
        if prefix > total {
            file.set_len(total)?;
            prefix = total;
        }
    }

    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: url.to_string(),
            size: total,
            etag: None,
            content_type: None,
            files: vec![],
        },
    });

    let final_size = match total {
        Some(total) if prefix >= total => prefix,
        Some(total) => {
            let connections = connection_count(budget.connections, total);
            if connections > 1 {
                match download_multipart(
                    sender,
                    client,
                    &task.task_id,
                    &url,
                    &config,
                    &file,
                    prefix,
                    total,
                    connections,
                    control,
                    progress,
                )? {
                    MultipartOutcome::Completed => total,
                    MultipartOutcome::RangeIgnored => {
                        // The server denied ranged GETs (200 instead of 206):
                        // restart from byte zero as a single stream.
                        file.set_len(0)?;
                        download_single(
                            sender,
                            client,
                            &task.task_id,
                            &url,
                            &config,
                            &file,
                            0,
                            control,
                            progress,
                            Some(total),
                        )?
                    }
                }
            } else {
                download_single(
                    sender,
                    client,
                    &task.task_id,
                    &url,
                    &config,
                    &file,
                    prefix,
                    control,
                    progress,
                    Some(total),
                )?
            }
        }
        None => download_single(
            sender,
            client,
            &task.task_id,
            &url,
            &config,
            &file,
            prefix,
            control,
            progress,
            None,
        )?,
    };

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(S3Error::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(&file_path);
            return Err(S3Error::ChecksumMismatch);
        }
    }

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
        plugin_state: None,
    })?;

    Ok(())
}

fn head_size(client: &Client, url: &Url, config: &Config) -> Option<u64> {
    let response = send(client, "HEAD", url, config, None).ok()?;
    if !response.status().is_success() {
        return None;
    }
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn connection_count(budget: u32, total: u64) -> usize {
    if total < MULTIPART_THRESHOLD {
        return 1;
    }
    budget.clamp(1, MAX_CONNECTIONS) as usize
}

#[allow(clippy::too_many_arguments)]
fn download_single(
    sender: &PluginSender,
    client: &Client,
    task_id: &str,
    url: &Url,
    config: &Config,
    file: &File,
    mut done: u64,
    control: &TaskControl,
    progress: &SharedProgress,
    known_total: Option<u64>,
) -> Result<u64, S3Error> {
    let range = (done > 0).then(|| format!("bytes={done}-"));
    let mut response = send(client, "GET", url, config, range.as_deref())?;
    let status = response.status();
    if !status.is_success() {
        return Err(S3Error::HttpStatus(status.as_u16()));
    }
    if done > 0 && status == StatusCode::OK {
        // The server ignored the resume Range; overwrite from the start.
        file.set_len(0)?;
        done = 0;
    }

    let total = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|length| length + done)
        .or(known_total);

    let mut snapshot = progress.lock().unwrap().clone();
    snapshot.downloaded = done;
    snapshot.total = total;
    snapshot.connections = 1;
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut offset = done;
    let mut buffer = vec![0_u8; CHUNK];

    loop {
        if !control.wait_until_running() {
            return Err(S3Error::Canceled);
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        write_all_at(file, &buffer[..read], offset)?;
        offset += read as u64;

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            snapshot.downloaded = offset;
            snapshot.seq = snapshot.seq.saturating_add(1);
            *progress.lock().unwrap() = snapshot.clone();
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task_id.to_string(),
                seq: snapshot.seq,
                downloaded: offset,
                total,
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                chunks_done: None,
                chunks_total: None,
            });
            last_progress = Instant::now();
        }
    }

    Ok(offset)
}

#[derive(Clone, Copy)]
struct Segment {
    /// Inclusive first byte.
    start: u64,
    /// Exclusive last byte.
    end: u64,
}

impl Segment {
    fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

fn plan_segments(prefix: u64, total: u64, connections: usize) -> Vec<Segment> {
    let remaining = total.saturating_sub(prefix);
    if remaining == 0 {
        return Vec::new();
    }
    let connections = connections.max(1) as u64;
    let chunk = remaining.div_ceil(connections).max(MIN_SEGMENT);
    let mut segments = Vec::new();
    let mut start = prefix;
    while start < total {
        let end = (start + chunk).min(total);
        segments.push(Segment { start, end });
        start = end;
    }
    segments
}

enum MultipartOutcome {
    Completed,
    RangeIgnored,
}

/// Download `[prefix, total)` with one worker per segment.
///
/// Workers write directly at their offsets, so the file may temporarily grow
/// past the contiguous prefix. On failure or cancellation the file is
/// truncated back to the contiguous prefix, which keeps the "resume from file
/// length" contract intact.
#[allow(clippy::too_many_arguments)]
fn download_multipart(
    sender: &PluginSender,
    client: &Client,
    task_id: &str,
    url: &Url,
    config: &Arc<Config>,
    file: &File,
    prefix: u64,
    total: u64,
    connections: usize,
    control: &Arc<TaskControl>,
    progress: &SharedProgress,
) -> Result<MultipartOutcome, S3Error> {
    let segments = plan_segments(prefix, total, connections);
    if segments.is_empty() {
        return Ok(MultipartOutcome::Completed);
    }

    let file = Arc::new(file.try_clone()?);
    let stop = Arc::new(AtomicBool::new(false));
    let received = Arc::new(AtomicU64::new(0));
    let seg_done: Vec<Arc<AtomicU64>> = segments
        .iter()
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let (result_tx, result_rx) = mpsc::channel();
    let mut workers = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().copied().enumerate() {
        let client = client.clone();
        let url = url.clone();
        let config = Arc::clone(config);
        let control = Arc::clone(control);
        let file = file.clone();
        let stop = stop.clone();
        let received = received.clone();
        let done = seg_done[index].clone();
        let result_tx = result_tx.clone();
        workers.push(thread::spawn(move || {
            let result = fetch_segment(
                &client, &url, &config, &file, segment, &stop, &control, &received, &done,
            );
            let _ = result_tx.send(result);
        }));
    }
    drop(result_tx);

    let mut snapshot = progress.lock().unwrap().clone();
    snapshot.downloaded = prefix;
    snapshot.total = Some(total);
    snapshot.connections = segments.len() as u32;
    snapshot.chunks_done = 0;
    snapshot.chunks_total = segments.len() as u32;

    let mut active = workers.len();
    let mut first_error: Option<S3Error> = None;
    let mut range_ignored = false;
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut last_bytes = prefix;
    let mut last_speed_time = Instant::now();

    while active > 0 {
        if control.is_canceled() {
            stop.store(true, Ordering::SeqCst);
        }
        loop {
            match result_rx.try_recv() {
                Ok(Ok(())) => active = active.saturating_sub(1),
                Ok(Err(S3Error::Canceled)) => active = active.saturating_sub(1),
                Ok(Err(S3Error::RangeIgnored)) => {
                    active = active.saturating_sub(1);
                    range_ignored = true;
                    stop.store(true, Ordering::SeqCst);
                }
                Ok(Err(error)) => {
                    active = active.saturating_sub(1);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    stop.store(true, Ordering::SeqCst);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    active = 0;
                    break;
                }
            }
        }
        if active == 0 {
            break;
        }

        let now = Instant::now();
        if now.duration_since(last_progress) >= PROGRESS_INTERVAL {
            let bytes = prefix + received.load(Ordering::Relaxed);
            let elapsed = now.duration_since(last_speed_time).as_secs_f64();
            let speed = if elapsed > 0.0 && !control.is_paused() {
                (bytes.saturating_sub(last_bytes) as f64 / elapsed) as u64
            } else {
                0
            };
            last_bytes = bytes;
            last_speed_time = now;
            last_progress = now;

            let done_count = segments
                .iter()
                .zip(&seg_done)
                .filter(|(segment, done)| done.load(Ordering::Relaxed) >= segment.len())
                .count() as u32;

            snapshot.downloaded = bytes;
            snapshot.speed_bps = speed;
            snapshot.chunks_done = done_count;
            snapshot.seq = snapshot.seq.saturating_add(1);
            *progress.lock().unwrap() = snapshot.clone();
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task_id.to_string(),
                seq: snapshot.seq,
                downloaded: bytes,
                total: Some(total),
                speed_bps: speed,
                eta_secs: eta(bytes, total, speed),
                connections: if control.is_paused() {
                    0
                } else {
                    segments.len() as u32
                },
                chunks_done: Some(done_count),
                chunks_total: Some(segments.len() as u32),
            });
        }

        thread::sleep(Duration::from_millis(50));
    }

    stop.store(true, Ordering::SeqCst);
    for worker in workers {
        let _ = worker.join();
    }

    if range_ignored {
        return Ok(MultipartOutcome::RangeIgnored);
    }
    if let Some(error) = first_error {
        truncate_to_prefix(&file, prefix, &segments, &seg_done)?;
        return Err(error);
    }
    if control.is_canceled() {
        truncate_to_prefix(&file, prefix, &segments, &seg_done)?;
        return Err(S3Error::Canceled);
    }
    Ok(MultipartOutcome::Completed)
}

#[allow(clippy::too_many_arguments)]
fn fetch_segment(
    client: &Client,
    url: &Url,
    config: &Config,
    file: &File,
    segment: Segment,
    stop: &AtomicBool,
    control: &TaskControl,
    received: &AtomicU64,
    done: &AtomicU64,
) -> Result<(), S3Error> {
    if stop.load(Ordering::Relaxed) {
        return Err(S3Error::Canceled);
    }
    if !control.wait_until_running() {
        return Err(S3Error::Canceled);
    }

    let range = format!("bytes={}-{}", segment.start, segment.end - 1);
    let mut response = send(client, "GET", url, config, Some(&range))?;
    match response.status() {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::OK => return Err(S3Error::RangeIgnored),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            return Err(S3Error::HttpStatus(
                StatusCode::RANGE_NOT_SATISFIABLE.as_u16(),
            ))
        }
        status if status.is_success() => return Err(S3Error::RangeIgnored),
        status => return Err(S3Error::HttpStatus(status.as_u16())),
    }

    let mut offset = segment.start;
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(S3Error::Canceled);
        }
        if !control.wait_until_running() {
            return Err(S3Error::Canceled);
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let next = offset + read as u64;
        if next > segment.end {
            return Err(S3Error::Protocol(format!(
                "server sent more bytes than requested for {range}"
            )));
        }
        write_all_at(file, &buffer[..read], offset)?;
        offset = next;
        received.fetch_add(read as u64, Ordering::Relaxed);
        done.fetch_add(read as u64, Ordering::Relaxed);
    }

    if offset - segment.start != segment.len() {
        return Err(S3Error::Protocol(format!(
            "short read for {range}: got {} bytes",
            offset - segment.start
        )));
    }
    Ok(())
}

fn truncate_to_prefix(
    file: &File,
    prefix: u64,
    segments: &[Segment],
    seg_done: &[Arc<AtomicU64>],
) -> Result<(), S3Error> {
    let mut committed = prefix;
    for (segment, done) in segments.iter().zip(seg_done) {
        let done = done.load(Ordering::Relaxed);
        if done < segment.len() {
            break;
        }
        committed += done;
    }
    file.set_len(committed)?;
    Ok(())
}

fn eta(downloaded: u64, total: u64, speed: u64) -> Option<u64> {
    if speed == 0 {
        return None;
    }
    Some(total.saturating_sub(downloaded) / speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_the_missing_range() {
        let segments = plan_segments(1024, 10 * 1024 * 1024, 4);
        assert_eq!(segments.first().unwrap().start, 1024);
        assert_eq!(segments.last().unwrap().end, 10 * 1024 * 1024);
        for pair in segments.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        let covered: u64 = segments.iter().map(|segment| segment.len()).sum();
        assert_eq!(covered, 10 * 1024 * 1024 - 1024);
    }

    #[test]
    fn small_objects_use_one_connection() {
        assert_eq!(connection_count(8, MULTIPART_THRESHOLD - 1), 1);
        assert_eq!(connection_count(8, MULTIPART_THRESHOLD), 8);
        assert_eq!(connection_count(0, MULTIPART_THRESHOLD), 1);
        assert_eq!(connection_count(99, MULTIPART_THRESHOLD), 8);
    }
}
