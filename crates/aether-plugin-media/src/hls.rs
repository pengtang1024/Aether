//! HLS playlist parsing, AES-128 decryption and segment download.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aes::Aes128;
use aether_plugin_sdk::fsutil::{sanitize_filename, write_all_at};
use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, FileInfo, Metadata, PluginEvent, PluginSender, TaskPaths, TaskSpec,
};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::MediaError;
use crate::http;

const STATE_FILE: &str = ".aether-hls-state.json";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

const LIVE_INTERVAL_DEFAULT_MS: u64 = 1_000;
const LIVE_INTERVAL_MIN_MS: u64 = 500;
const LIVE_INTERVAL_MAX_MS: u64 = 10_000;
const LIVE_IDLE_POLLS_DEFAULT: u64 = 3;
/// Consecutive playlist refresh failures tolerated before the task fails.
const LIVE_PLAYLIST_MAX_FAILURES: u32 = 5;

#[derive(Debug, Clone)]
struct HlsSegment {
    url: Url,
    range: Option<(u64, u64)>,
    sequence: u64,
}

#[derive(Debug, Clone)]
struct HlsKey {
    uri: Url,
    iv: Option<[u8; 16]>,
}

#[derive(Debug, Clone)]
struct HlsMedia {
    segments: Vec<HlsSegment>,
    map: Option<Url>,
    key: Option<HlsKey>,
    endlist: bool,
}

#[derive(Debug, Clone)]
struct HlsVariant {
    bandwidth: u64,
    uri: Url,
}

enum HlsPlaylist {
    Master(Vec<HlsVariant>),
    Media(HlsMedia),
}

pub struct HlsProbe {
    pub supported: bool,
    pub filename: Option<String>,
    pub reason: Option<String>,
}

pub fn probe(client: &reqwest::blocking::Client, input: &str) -> HlsProbe {
    let Ok(url) = Url::parse(input) else {
        return HlsProbe {
            supported: false,
            filename: None,
            reason: Some("invalid URL".into()),
        };
    };
    if !is_hls_url(&url) {
        return HlsProbe {
            supported: false,
            filename: None,
            reason: None,
        };
    }

    match fetch_media(client, &url) {
        Ok((media, playlist_url)) => HlsProbe {
            supported: true,
            filename: Some(output_filename(&playlist_url, media.map.is_some())),
            reason: None,
        },
        Err(error) => HlsProbe {
            supported: true,
            filename: output_filename(&url, false).into(),
            reason: Some(error.to_string()),
        },
    }
}

pub fn is_hls_url(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".m3u8")
}

pub fn download(
    sender: PluginSender,
    client: reqwest::blocking::Client,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    match download_inner(&sender, &client, &task, &paths, &control, &progress) {
        Ok(()) => {}
        Err(MediaError::Canceled) => {}
        Err(error) => {
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: error.to_plugin_error(),
            });
        }
    }
}

fn download_inner(
    sender: &PluginSender,
    client: &reqwest::blocking::Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
) -> Result<(), MediaError> {
    if control.is_canceled() {
        return Err(MediaError::Canceled);
    }

    let url = Url::parse(&task.url)?;
    let (media, playlist_url) = fetch_media(client, &url)?;
    if media.endlist {
        download_vod(
            sender,
            client,
            task,
            paths,
            control,
            progress,
            media,
            playlist_url,
        )
    } else {
        download_live(
            sender,
            client,
            task,
            paths,
            control,
            progress,
            media,
            playlist_url,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn download_vod(
    sender: &PluginSender,
    client: &reqwest::blocking::Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
    media: HlsMedia,
    playlist_url: Url,
) -> Result<(), MediaError> {
    fs::create_dir_all(&paths.staging)?;
    let has_map = media.map.is_some();
    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .unwrap_or_else(|| output_filename(&playlist_url, has_map));
    let file_path = paths.staging.join(&filename);

    let mut jobs = Vec::new();
    if let Some(map) = &media.map {
        jobs.push(SegmentJob {
            url: map.clone(),
            range: None,
            sequence: 0,
            encrypted: false,
        });
    }
    for segment in &media.segments {
        jobs.push(SegmentJob {
            url: segment.url.clone(),
            range: segment.range,
            sequence: segment.sequence,
            encrypted: media.key.is_some(),
        });
    }
    if jobs.is_empty() {
        return Err(MediaError::Protocol("playlist has no segments".into()));
    }

    let key = match &media.key {
        Some(key) => {
            let bytes = http::get_bytes(client, &key.uri)?;
            if bytes.len() != 16 {
                return Err(MediaError::Protocol(format!(
                    "AES-128 key must be 16 bytes, got {}",
                    bytes.len()
                )));
            }
            Some((bytes, key.iv))
        }
        None => None,
    };

    let fingerprint = fingerprint(&playlist_url, &jobs);
    let state_path = paths.staging.join(STATE_FILE);
    let mut state = read_state(&state_path)
        .ok()
        .flatten()
        .filter(|state| {
            state.playlist_url == playlist_url.as_str()
                && state.fingerprint == fingerprint
                && state.filename == filename
        })
        .unwrap_or(HlsState {
            playlist_url: playlist_url.to_string(),
            fingerprint,
            filename: filename.clone(),
            completed: 0,
            bytes: 0,
        });

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&file_path)?;
    let existing = file.metadata()?.len();
    if state.completed > jobs.len() || existing != state.bytes {
        state.completed = 0;
        state.bytes = 0;
        file.set_len(0)?;
    }

    write_state(&state_path, &state)?;
    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: playlist_url.to_string(),
            size: None,
            etag: None,
            content_type: Some("application/vnd.apple.mpegurl".into()),
            files: vec![],
        },
    });

    let mut snapshot = ProgressSnapshot {
        downloaded: state.bytes,
        total: None,
        connections: 1,
        chunks_done: state.completed as u32,
        chunks_total: jobs.len() as u32,
        ..Default::default()
    };
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut offset = state.bytes;

    for (index, job) in jobs.iter().enumerate().skip(state.completed) {
        if !control.wait_until_running() {
            write_state(&state_path, &state)?;
            return Err(MediaError::Canceled);
        }

        let mut bytes = match job.range {
            Some((start, end)) => {
                let mut response = http::get_bytes_range(client, &job.url, start, end)?;
                let mut buffer = Vec::new();
                response.read_to_end(&mut buffer)?;
                buffer
            }
            None => http::get_bytes(client, &job.url)?,
        };

        if job.encrypted {
            let (key, iv) = key
                .as_ref()
                .ok_or_else(|| MediaError::Protocol("missing AES key".into()))?;
            let iv = iv.unwrap_or_else(|| sequence_iv(job.sequence));
            bytes = decrypt(bytes, key, &iv)?;
        }

        write_all_at(&file, &bytes, offset)?;
        offset += bytes.len() as u64;
        state.completed = index + 1;
        state.bytes = offset;
        write_state(&state_path, &state)?;

        if last_progress.elapsed() >= PROGRESS_INTERVAL || index + 1 == jobs.len() {
            snapshot.downloaded = offset;
            snapshot.chunks_done = state.completed as u32;
            snapshot.seq = snapshot.seq.saturating_add(1);
            *progress.lock().unwrap() = snapshot.clone();
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq: snapshot.seq,
                downloaded: offset,
                total: None,
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                chunks_done: Some(state.completed as u32),
                chunks_total: Some(jobs.len() as u32),
            });
            last_progress = Instant::now();
        }
    }

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(MediaError::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(&file_path);
            let _ = fs::remove_file(&state_path);
            return Err(MediaError::ChecksumMismatch);
        }
    }

    let _ = fs::remove_file(&state_path);
    // Optional container remux (`transcode_to`): runs after the download is
    // complete and never fails the task — on error the original file is
    // kept and the note lands in plugin_state.
    let downloaded = FileInfo {
        relative_path: filename,
        size: offset,
        checksum: Some(Checksum {
            algorithm: ChecksumAlgorithm::Sha256,
            value: checksum,
        }),
    };
    let (finished, transcode_note) =
        crate::transcode::maybe_remux(sender, task, &paths.staging, &downloaded, progress);
    let mut plugin_state = serde_json::json!({
        "playlist": playlist_url.to_string(),
        "segments": jobs.len(),
    });
    if let Some(note) = transcode_note {
        plugin_state["transcode"] = serde_json::json!({"ok": false, "note": note});
    }
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![finished],
        plugin_state: Some(plugin_state),
    })?;

    Ok(())
}

/// Live (no `#EXT-X-ENDLIST`) behaviour: refresh the media playlist, append
/// only segments that were not written yet, and keep the task alive until
/// the publisher ends the stream or the task is canceled.
///
/// `transcode_to` is intentionally skipped: a live stream has no stable end
/// to remux, so the note is reported through `plugin_state` instead.
#[allow(clippy::too_many_arguments)]
fn download_live(
    sender: &PluginSender,
    client: &reqwest::blocking::Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
    mut media: HlsMedia,
    mut playlist_url: Url,
) -> Result<(), MediaError> {
    fs::create_dir_all(&paths.staging)?;
    let has_map = media.map.is_some();
    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .unwrap_or_else(|| output_filename(&playlist_url, has_map));
    let file_path = paths.staging.join(&filename);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&file_path)?;

    let config = LiveConfig::from_task(task);
    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: playlist_url.to_string(),
            size: None,
            etag: None,
            content_type: Some("application/vnd.apple.mpegurl".into()),
            files: vec![],
        },
    });

    let mut writer = LiveWriter {
        file: &file,
        client,
        seen: HashSet::new(),
        keys: HashMap::new(),
        map_written: None,
        offset: 0,
        segments: 0,
    };
    let mut snapshot = ProgressSnapshot {
        total: None,
        connections: 1,
        ..Default::default()
    };
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut first_batch = true;
    let mut idle_polls = 0_u64;
    let mut failures = 0_u32;

    loop {
        match writer.append(&media, control) {
            Ok(added) => {
                idle_polls = if added > 0 { 0 } else { idle_polls + 1 };
                snapshot.downloaded = writer.offset;
                snapshot.chunks_done = writer.segments as u32;
                let throttled = last_progress.elapsed() >= PROGRESS_INTERVAL;
                if added > 0 && (first_batch || throttled || media.endlist) {
                    emit_live_progress(sender, task, &mut snapshot, progress);
                    last_progress = Instant::now();
                    first_batch = false;
                }
            }
            Err(MediaError::Canceled) => {
                return live_canceled(sender, task);
            }
            Err(error) => return Err(error),
        }

        if media.endlist {
            break;
        }

        if !sleep_until_poll(control, config.interval) {
            return live_canceled(sender, task);
        }

        match fetch_media(client, &playlist_url) {
            Ok((next, next_url)) => {
                media = next;
                playlist_url = next_url;
                failures = 0;
            }
            Err(error) => {
                failures += 1;
                if failures > LIVE_PLAYLIST_MAX_FAILURES {
                    return Err(MediaError::Network(format!(
                        "live playlist refresh failed {failures} times in a row: {error}"
                    )));
                }
            }
        }
    }

    if writer.segments == 0 && writer.map_written.is_none() {
        return Err(MediaError::Protocol("playlist has no segments".into()));
    }

    file.sync_all()?;
    snapshot.downloaded = writer.offset;
    snapshot.chunks_done = writer.segments as u32;
    emit_live_progress(sender, task, &mut snapshot, progress);

    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(MediaError::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(&file_path);
            return Err(MediaError::ChecksumMismatch);
        }
    }

    let finished = FileInfo {
        relative_path: filename,
        size: writer.offset,
        checksum: Some(Checksum {
            algorithm: ChecksumAlgorithm::Sha256,
            value: checksum,
        }),
    };
    let mut plugin_state = serde_json::json!({
        "playlist": playlist_url.to_string(),
        "segments": writer.segments,
        "live": true,
        "idle_polls": idle_polls,
        "idle_poll_limit": config.idle_polls,
    });
    if crate::transcode::target_container(task).is_some() {
        plugin_state["transcode"] = serde_json::json!({
            "ok": false,
            "note": "transcode_to is skipped for live HLS",
        });
    }
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![finished],
        plugin_state: Some(plugin_state),
    })?;

    Ok(())
}

/// A canceled live task reports `Failed`/`Canceled` so the host can finish
/// the task deterministically; the VOD path keeps its silent cancellation.
fn live_canceled(sender: &PluginSender, task: &TaskSpec) -> Result<(), MediaError> {
    let _ = sender.send(&PluginEvent::Failed {
        task_id: task.task_id.clone(),
        error: MediaError::Canceled.to_plugin_error(),
    });
    Ok(())
}

struct LiveConfig {
    interval: Duration,
    idle_polls: u64,
}

impl LiveConfig {
    fn from_task(task: &TaskSpec) -> Self {
        let interval_ms = setting_u64(&task.settings, "hls_live_interval_ms")
            .unwrap_or(LIVE_INTERVAL_DEFAULT_MS)
            .clamp(LIVE_INTERVAL_MIN_MS, LIVE_INTERVAL_MAX_MS);
        let idle_polls =
            setting_u64(&task.settings, "hls_live_idle_polls").unwrap_or(LIVE_IDLE_POLLS_DEFAULT);
        Self {
            interval: Duration::from_millis(interval_ms),
            idle_polls,
        }
    }
}

fn setting_u64(settings: &serde_json::Value, key: &str) -> Option<u64> {
    settings.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    })
}

struct LiveWriter<'a> {
    file: &'a std::fs::File,
    client: &'a reqwest::blocking::Client,
    seen: HashSet<String>,
    keys: HashMap<String, Vec<u8>>,
    map_written: Option<String>,
    offset: u64,
    segments: u64,
}

impl LiveWriter<'_> {
    /// Append a playlist's not-yet-written parts to the output file. Returns
    /// the number of new media segments (the init map is not counted).
    fn append(&mut self, media: &HlsMedia, control: &TaskControl) -> Result<u64, MediaError> {
        let mut added = 0_u64;

        if let Some(map) = &media.map {
            let identity = map.as_str().to_string();
            if self.map_written.as_deref() != Some(identity.as_str()) {
                let bytes = http::get_bytes(self.client, map)?;
                write_all_at(self.file, &bytes, self.offset)?;
                self.offset += bytes.len() as u64;
                self.map_written = Some(identity);
            }
        }

        for segment in &media.segments {
            if !control.wait_until_running() {
                return Err(MediaError::Canceled);
            }
            if !self.seen.insert(segment_identity(segment)) {
                continue;
            }

            let mut bytes = match segment.range {
                Some((start, end)) => {
                    let mut response =
                        http::get_bytes_range(self.client, &segment.url, start, end)?;
                    let mut buffer = Vec::new();
                    response.read_to_end(&mut buffer)?;
                    buffer
                }
                None => http::get_bytes(self.client, &segment.url)?,
            };

            if let Some(key) = &media.key {
                let key_bytes = self.key_bytes(key)?;
                let iv = key.iv.unwrap_or_else(|| sequence_iv(segment.sequence));
                bytes = decrypt(bytes, &key_bytes, &iv)?;
            }

            write_all_at(self.file, &bytes, self.offset)?;
            self.offset += bytes.len() as u64;
            self.segments += 1;
            added += 1;
        }

        Ok(added)
    }

    fn key_bytes(&mut self, key: &HlsKey) -> Result<Vec<u8>, MediaError> {
        let identity = key.uri.to_string();
        if let Some(bytes) = self.keys.get(&identity) {
            return Ok(bytes.clone());
        }
        let bytes = http::get_bytes(self.client, &key.uri)?;
        if bytes.len() != 16 {
            return Err(MediaError::Protocol(format!(
                "AES-128 key must be 16 bytes, got {}",
                bytes.len()
            )));
        }
        self.keys.insert(identity, bytes.clone());
        Ok(bytes)
    }
}

fn segment_identity(segment: &HlsSegment) -> String {
    match segment.range {
        Some((start, end)) => format!("{}|{}|{start}-{end}", segment.sequence, segment.url),
        None => format!("{}|{}", segment.sequence, segment.url),
    }
}

fn emit_live_progress(
    sender: &PluginSender,
    task: &TaskSpec,
    snapshot: &mut ProgressSnapshot,
    progress: &SharedProgress,
) {
    snapshot.seq = snapshot.seq.saturating_add(1);
    *progress.lock().unwrap() = snapshot.clone();
    let _ = sender.send(&PluginEvent::Progress {
        task_id: task.task_id.clone(),
        seq: snapshot.seq,
        downloaded: snapshot.downloaded,
        total: None,
        speed_bps: 0,
        eta_secs: None,
        connections: 1,
        chunks_done: Some(snapshot.chunks_done),
        chunks_total: None,
    });
}

/// Sleep for the poll interval in short slices so pause/cancel are honoured
/// promptly. Returns false when the task was canceled.
fn sleep_until_poll(control: &TaskControl, interval: Duration) -> bool {
    let mut remaining = interval;
    while !remaining.is_zero() {
        if !control.wait_until_running() {
            return false;
        }
        let step = remaining.min(Duration::from_millis(100));
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !control.is_canceled()
}

struct SegmentJob {
    url: Url,
    range: Option<(u64, u64)>,
    sequence: u64,
    encrypted: bool,
}

fn fetch_media(
    client: &reqwest::blocking::Client,
    url: &Url,
) -> Result<(HlsMedia, Url), MediaError> {
    let mut current = url.clone();
    for _ in 0..2 {
        let text = http::get_text(client, &current)?;
        match parse_playlist(&text, &current)? {
            HlsPlaylist::Media(media) => return Ok((media, current)),
            HlsPlaylist::Master(variants) => {
                let variant = variants
                    .into_iter()
                    .max_by_key(|variant| variant.bandwidth)
                    .ok_or_else(|| {
                        MediaError::Protocol("master playlist has no variants".into())
                    })?;
                current = variant.uri;
            }
        }
    }
    Err(MediaError::Protocol(
        "nested master playlists are not supported".into(),
    ))
}

fn parse_playlist(text: &str, base: &Url) -> Result<HlsPlaylist, MediaError> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err(MediaError::Protocol("not an HLS playlist".into()));
    }

    let mut lines = text.lines().map(str::trim);
    let mut variants = Vec::new();
    let mut segments = Vec::new();
    let mut map = None;
    let mut key = None;
    let mut sequence = 0_u64;
    let mut pending_range = None;
    let mut pending_inf = false;
    let mut endlist = false;

    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attrs(rest);
            let method = attrs.get("METHOD").map(String::as_str).unwrap_or("NONE");
            if method.eq_ignore_ascii_case("NONE") {
                key = None;
            } else if method.eq_ignore_ascii_case("AES-128") {
                let uri = attrs
                    .get("URI")
                    .ok_or_else(|| MediaError::Protocol("AES key has no URI".into()))?;
                key = Some(HlsKey {
                    uri: resolve(base, uri)?,
                    iv: attrs.get("IV").and_then(|iv| parse_iv(iv)),
                });
            } else {
                return Err(MediaError::Protocol(format!(
                    "unsupported HLS encryption method {method}"
                )));
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_attrs(rest);
            if let Some(uri) = attrs.get("URI") {
                map = Some(resolve(base, uri)?);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            if let Some((length, offset)) = rest.split_once('@') {
                if let (Ok(length), Ok(offset)) = (length.parse::<u64>(), offset.parse::<u64>()) {
                    pending_range = Some((offset, offset + length - 1));
                }
            }
        } else if line.starts_with("#EXTINF:") {
            pending_inf = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attrs(rest);
            let bandwidth = attrs
                .get("BANDWIDTH")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            while let Some(next) = lines.next() {
                if next.is_empty() || next.starts_with('#') {
                    continue;
                }
                variants.push(HlsVariant {
                    bandwidth,
                    uri: resolve(base, next)?,
                });
                break;
            }
        } else if line.starts_with('#') {
            if line.starts_with("#EXT-X-ENDLIST") {
                endlist = true;
            }
        } else if pending_inf {
            segments.push(HlsSegment {
                url: resolve(base, line)?,
                range: pending_range.take(),
                sequence,
            });
            sequence += 1;
            pending_inf = false;
        }
    }

    if !variants.is_empty() {
        Ok(HlsPlaylist::Master(variants))
    } else {
        Ok(HlsPlaylist::Media(HlsMedia {
            segments,
            map,
            key,
            endlist,
        }))
    }
}

fn resolve(base: &Url, value: &str) -> Result<Url, MediaError> {
    Ok(base.join(value.trim())?)
}

fn parse_attrs(value: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut current = String::new();
    let mut quoted = false;

    for ch in value.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            ',' if !quoted => {
                insert_attr(&mut result, &current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    insert_attr(&mut result, &current);
    result
}

fn insert_attr(result: &mut HashMap<String, String>, part: &str) {
    if let Some((name, value)) = part.split_once('=') {
        result.insert(
            name.trim().to_ascii_uppercase(),
            value.trim().trim_matches('"').to_string(),
        );
    }
}

fn parse_iv(value: &str) -> Option<[u8; 16]> {
    let hex = value.trim().trim_start_matches("0x");
    let padded = format!("{hex:0>32}");
    if padded.len() != 32 {
        return None;
    }
    let mut iv = [0_u8; 16];
    for (index, byte) in iv.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&padded[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(iv)
}

fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

fn decrypt(data: Vec<u8>, key: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>, MediaError> {
    type Decryptor = cbc::Decryptor<Aes128>;
    let mut buffer = data;
    let decryptor = Decryptor::new_from_slices(key, iv)
        .map_err(|error| MediaError::Protocol(format!("AES init: {error}")))?;
    let plaintext = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| MediaError::Protocol("AES-128 padding error".into()))?;
    Ok(plaintext.to_vec())
}

fn output_filename(url: &Url, has_map: bool) -> String {
    let stem = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("media")
        .trim_end_matches(".m3u8");
    let stem = if stem.is_empty() { "media" } else { stem };
    format!("{stem}.{}", if has_map { "mp4" } else { "ts" })
}

fn fingerprint(playlist: &Url, jobs: &[SegmentJob]) -> String {
    let mut material = playlist.to_string();
    for job in jobs {
        material.push('|');
        material.push_str(job.url.as_str());
        if let Some((start, end)) = job.range {
            material.push_str(&format!("#{start}-{end}"));
        }
    }
    aether_plugin_sdk::checksum::sha256_bytes(material.as_bytes())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HlsState {
    playlist_url: String,
    fingerprint: String,
    filename: String,
    completed: usize,
    bytes: u64,
}

fn read_state(path: &Path) -> Result<Option<HlsState>, MediaError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MediaError::Io(error)),
    }
}

fn write_state(path: &Path, state: &HlsState) -> Result<(), MediaError> {
    fs::write(path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.0,\nseg0.ts\n#EXTINF:2.0,\nseg1.ts\n#EXT-X-ENDLIST\n";

    #[test]
    fn parses_media_playlist() {
        let base = Url::parse("https://example.com/media/index.m3u8").unwrap();
        let HlsPlaylist::Media(media) = parse_playlist(PLAYLIST, &base).unwrap() else {
            panic!("expected media playlist");
        };
        assert_eq!(media.segments.len(), 2);
        assert_eq!(
            media.segments[0].url.as_str(),
            "https://example.com/media/seg0.ts"
        );
        assert!(media.endlist);
    }

    #[test]
    fn parses_master_bandwidth() {
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000\nhigh.m3u8\n";
        let base = Url::parse("https://example.com/master.m3u8").unwrap();
        let HlsPlaylist::Master(variants) = parse_playlist(text, &base).unwrap() else {
            panic!("expected master playlist");
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].bandwidth, 5000);
        assert_eq!(variants[1].uri.as_str(), "https://example.com/high.m3u8");
    }

    #[test]
    fn parses_iv() {
        assert_eq!(
            parse_iv("0x000102030405060708090A0B0C0D0E0F").unwrap(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn playlist_without_endlist_is_live() {
        let text =
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:2.0,\nseg7.ts\n";
        let base = Url::parse("https://example.com/live/index.m3u8").unwrap();
        let HlsPlaylist::Media(media) = parse_playlist(text, &base).unwrap() else {
            panic!("expected media playlist");
        };
        assert!(!media.endlist);
        assert_eq!(media.segments.len(), 1);
        assert_eq!(media.segments[0].sequence, 7);
    }

    #[test]
    fn segment_identity_distinguishes_ranges() {
        let url = Url::parse("https://example.com/seg.ts").unwrap();
        let whole = HlsSegment {
            url: url.clone(),
            range: None,
            sequence: 3,
        };
        let ranged = HlsSegment {
            url,
            range: Some((0, 99)),
            sequence: 3,
        };
        assert_ne!(segment_identity(&whole), segment_identity(&ranged));
    }

    #[test]
    fn live_settings_are_clamped() {
        let task = |settings: serde_json::Value| TaskSpec {
            task_id: "t".into(),
            url: "https://example.com/index.m3u8".into(),
            filename: None,
            settings,
            plugin_state: None,
            expected_size: None,
            checksum: None,
            proxy: None,
        };

        let default = LiveConfig::from_task(&task(serde_json::json!({})));
        assert_eq!(default.interval, Duration::from_millis(1_000));
        assert_eq!(default.idle_polls, 3);

        let low = LiveConfig::from_task(&task(serde_json::json!({"hls_live_interval_ms": 10})));
        assert_eq!(low.interval, Duration::from_millis(500));

        let high =
            LiveConfig::from_task(&task(serde_json::json!({"hls_live_interval_ms": 99_999})));
        assert_eq!(high.interval, Duration::from_millis(10_000));

        let custom = LiveConfig::from_task(&task(serde_json::json!({
            "hls_live_interval_ms": "750",
            "hls_live_idle_polls": 8,
        })));
        assert_eq!(custom.interval, Duration::from_millis(750));
        assert_eq!(custom.idle_polls, 8);
    }
}
