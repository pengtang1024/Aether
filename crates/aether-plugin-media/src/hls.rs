//! HLS playlist parsing, AES-128 decryption and segment download.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
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
            reason: (!media.endlist).then(|| "live HLS is not supported yet".into()),
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
    if !media.endlist {
        return Err(MediaError::Protocol("live HLS is not supported yet".into()));
    }

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
}
