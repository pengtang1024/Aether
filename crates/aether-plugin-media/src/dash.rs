//! Minimal MPEG-DASH VOD support: SegmentTemplate ($Number$/$Time$) and
//! SegmentList, first Period, highest-bandwidth Representation.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::{sanitize_filename, write_all_at};
use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, FileInfo, Metadata, PluginEvent, PluginSender, TaskPaths, TaskSpec,
};
use serde::Deserialize as DeriveDeserialize;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::MediaError;
use crate::http;

const STATE_FILE: &str = ".aether-dash-state.json";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

pub struct DashProbe {
    pub supported: bool,
    pub filename: Option<String>,
    pub reason: Option<String>,
}

pub fn is_dash_url(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".mpd")
}

pub fn probe(client: &reqwest::blocking::Client, input: &str) -> DashProbe {
    let Ok(url) = Url::parse(input) else {
        return DashProbe {
            supported: false,
            filename: None,
            reason: Some("invalid URL".into()),
        };
    };
    if !is_dash_url(&url) {
        return DashProbe {
            supported: false,
            filename: None,
            reason: None,
        };
    }

    match build_plan(client, &url) {
        Ok(plan) => DashProbe {
            supported: true,
            filename: Some(plan.filename),
            reason: None,
        },
        Err(error) => DashProbe {
            supported: true,
            filename: Some(output_filename(&url)),
            reason: Some(error.to_string()),
        },
    }
}

struct DashPlan {
    init: Option<Url>,
    segments: Vec<Url>,
    filename: String,
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

    let mpd_url = Url::parse(&task.url)?;
    let plan = build_plan(client, &mpd_url)?;
    fs::create_dir_all(&paths.staging)?;

    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .unwrap_or_else(|| plan.filename.clone());
    let file_path = paths.staging.join(&filename);
    let state_path = paths.staging.join(STATE_FILE);

    let fingerprint = fingerprint(&mpd_url, &plan);
    let mut state = read_state(&state_path)
        .ok()
        .flatten()
        .filter(|state| {
            state.mpd_url == mpd_url.as_str()
                && state.fingerprint == fingerprint
                && state.filename == filename
        })
        .unwrap_or(DashState {
            mpd_url: mpd_url.to_string(),
            fingerprint,
            filename: filename.clone(),
            completed: 0,
            bytes: 0,
        });

    let jobs = jobs(&plan);
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
            final_url: mpd_url.to_string(),
            size: None,
            etag: None,
            content_type: Some("application/dash+xml".into()),
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

    for (index, url) in jobs.iter().enumerate().skip(state.completed) {
        if !control.wait_until_running() {
            write_state(&state_path, &state)?;
            return Err(MediaError::Canceled);
        }

        let bytes = http::get_bytes(client, url)?;
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
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![FileInfo {
            relative_path: filename,
            size: offset,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: Some(serde_json::json!({
            "mpd": mpd_url.to_string(),
            "segments": jobs.len(),
        })),
    })?;

    Ok(())
}

fn jobs(plan: &DashPlan) -> Vec<Url> {
    let mut jobs = Vec::new();
    if let Some(init) = &plan.init {
        jobs.push(init.clone());
    }
    jobs.extend(plan.segments.iter().cloned());
    jobs
}

fn fingerprint(mpd: &Url, plan: &DashPlan) -> String {
    let mut material = mpd.to_string();
    if let Some(init) = &plan.init {
        material.push('|');
        material.push_str(init.as_str());
    }
    for segment in &plan.segments {
        material.push('|');
        material.push_str(segment.as_str());
    }
    aether_plugin_sdk::checksum::sha256_bytes(material.as_bytes())
}

fn build_plan(client: &reqwest::blocking::Client, mpd_url: &Url) -> Result<DashPlan, MediaError> {
    let text = http::get_text(client, mpd_url)?;
    let mpd: Mpd = quick_xml::de::from_str(&text)
        .map_err(|error| MediaError::Protocol(format!("invalid MPD: {error}")))?;

    if mpd.kind.as_deref() == Some("dynamic") {
        return Err(MediaError::Protocol(
            "live DASH is not supported yet".into(),
        ));
    }

    let mut base = join_base(mpd_url, mpd.base_url.as_ref());
    let period = mpd
        .periods
        .first()
        .ok_or_else(|| MediaError::Protocol("MPD has no Period".into()))?;
    base = join_base(&base, period.base_url.as_ref());
    let duration = period
        .duration
        .as_deref()
        .or(mpd.duration.as_deref())
        .and_then(parse_duration)
        .ok_or_else(|| MediaError::Protocol("MPD/Period has no duration".into()))?;

    let mut best: Option<(u64, &Representation)> = None;
    for adaptation in &period.adaptations {
        for representation in &adaptation.representations {
            let bandwidth = representation.bandwidth.unwrap_or(0);
            if best.is_none_or(|(current, _)| bandwidth > current) {
                best = Some((bandwidth, representation));
            }
        }
    }
    let (_, representation) =
        best.ok_or_else(|| MediaError::Protocol("MPD has no Representation".into()))?;
    let adaptation = period
        .adaptations
        .iter()
        .find(|adaptation| {
            adaptation
                .representations
                .iter()
                .any(|rep| std::ptr::eq(rep, representation))
        })
        .ok_or_else(|| MediaError::Protocol("representation has no AdaptationSet".into()))?;
    base = join_base(&base, adaptation.base_url.as_ref());
    base = join_base(&base, representation.base_url.as_ref());

    let template = representation
        .template
        .clone()
        .or_else(|| adaptation.template.clone());
    let list = representation.list.as_ref();

    let init = template
        .as_ref()
        .and_then(|template| template.initialization.as_deref())
        .map(|value| replace_template(value, representation, 0, 0))
        .or_else(|| {
            list.and_then(|list| list.initialization.as_ref())
                .and_then(|init| init.source.as_deref())
                .map(ToOwned::to_owned)
        })
        .map(|value| base.join(&value))
        .transpose()?;

    let mut segments = Vec::new();
    if let Some(list) = list {
        for segment in &list.segments {
            if let Some(media) = &segment.media {
                segments.push(base.join(media)?);
            }
        }
    } else if let Some(template) = &template {
        let media = template
            .media
            .as_deref()
            .ok_or_else(|| MediaError::Protocol("SegmentTemplate has no media".into()))?;
        let timescale = template.timescale.unwrap_or(1).max(1);
        let segment_duration = template
            .duration
            .ok_or_else(|| MediaError::Protocol("SegmentTemplate has no duration".into()))?;
        let count = ((duration * timescale as f64) / segment_duration as f64).ceil() as u64;
        if count == 0 {
            return Err(MediaError::Protocol("no DASH segments".into()));
        }
        let start = template.start_number.unwrap_or(1);
        let pto = template.presentation_time_offset.unwrap_or(0);
        for index in 0..count {
            let number = start + index;
            let time = index * segment_duration + pto;
            let value = replace_template(media, representation, number, time);
            segments.push(base.join(&value)?);
        }
    } else {
        return Err(MediaError::Protocol(
            "unsupported DASH addressing (need SegmentTemplate or SegmentList)".into(),
        ));
    }

    if segments.is_empty() {
        return Err(MediaError::Protocol("no DASH segments".into()));
    }

    Ok(DashPlan {
        init,
        segments,
        filename: output_filename(mpd_url),
    })
}

fn replace_template(
    value: &str,
    representation: &Representation,
    number: u64,
    time: u64,
) -> String {
    value
        .replace("$RepresentationID$", &representation.id)
        .replace(
            "$Bandwidth$",
            &representation.bandwidth.unwrap_or(0).to_string(),
        )
        .replace("$Number$", &number.to_string())
        .replace("$Time$", &time.to_string())
}

fn join_base(base: &Url, value: Option<&TextValue>) -> Url {
    match value {
        Some(value) => base
            .join(value.value.trim())
            .unwrap_or_else(|_| base.clone()),
        None => base.clone(),
    }
}

fn output_filename(url: &Url) -> String {
    let stem = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("media")
        .trim_end_matches(".mpd");
    format!("{}.mp4", if stem.is_empty() { "media" } else { stem })
}

fn parse_duration(value: &str) -> Option<f64> {
    let value = value.strip_prefix("PT")?;
    let mut number = String::new();
    let mut seconds = 0.0;
    for ch in value.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
        } else {
            let amount: f64 = number.parse().ok()?;
            number.clear();
            seconds += match ch {
                'H' => amount * 3600.0,
                'M' => amount * 60.0,
                'S' => amount,
                _ => return None,
            };
        }
    }
    Some(seconds)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DashState {
    mpd_url: String,
    fingerprint: String,
    filename: String,
    completed: usize,
    bytes: u64,
}

fn read_state(path: &Path) -> Result<Option<DashState>, MediaError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MediaError::Io(error)),
    }
}

fn write_state(path: &Path, state: &DashState) -> Result<(), MediaError> {
    fs::write(path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

#[derive(Debug, DeriveDeserialize)]
struct Mpd {
    #[serde(rename = "@mediaPresentationDuration")]
    duration: Option<String>,
    #[serde(rename = "@type")]
    kind: Option<String>,
    #[serde(rename = "BaseURL")]
    base_url: Option<TextValue>,
    #[serde(rename = "Period", default)]
    periods: Vec<Period>,
}

#[derive(Debug, DeriveDeserialize)]
struct Period {
    #[serde(rename = "@duration")]
    duration: Option<String>,
    #[serde(rename = "BaseURL")]
    base_url: Option<TextValue>,
    #[serde(rename = "AdaptationSet", default)]
    adaptations: Vec<AdaptationSet>,
}

#[derive(Debug, DeriveDeserialize)]
struct AdaptationSet {
    #[serde(rename = "BaseURL")]
    base_url: Option<TextValue>,
    #[serde(rename = "SegmentTemplate")]
    template: Option<SegmentTemplate>,
    #[serde(rename = "Representation", default)]
    representations: Vec<Representation>,
}

#[derive(Debug, DeriveDeserialize)]
struct Representation {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@bandwidth")]
    bandwidth: Option<u64>,
    #[serde(rename = "BaseURL")]
    base_url: Option<TextValue>,
    #[serde(rename = "SegmentTemplate")]
    template: Option<SegmentTemplate>,
    #[serde(rename = "SegmentList")]
    list: Option<SegmentList>,
}

#[derive(Debug, Clone, DeriveDeserialize)]
struct SegmentTemplate {
    #[serde(rename = "@media")]
    media: Option<String>,
    #[serde(rename = "@initialization")]
    initialization: Option<String>,
    #[serde(rename = "@timescale")]
    timescale: Option<u64>,
    #[serde(rename = "@duration")]
    duration: Option<u64>,
    #[serde(rename = "@startNumber")]
    start_number: Option<u64>,
    #[serde(rename = "@presentationTimeOffset")]
    presentation_time_offset: Option<u64>,
}

#[derive(Debug, DeriveDeserialize)]
struct SegmentList {
    #[serde(rename = "Initialization")]
    initialization: Option<Initialization>,
    #[serde(rename = "SegmentURL", default)]
    segments: Vec<SegmentUrl>,
}

#[derive(Debug, DeriveDeserialize)]
struct Initialization {
    #[serde(rename = "@sourceURL")]
    source: Option<String>,
}

#[derive(Debug, DeriveDeserialize)]
struct SegmentUrl {
    #[serde(rename = "@media")]
    media: Option<String>,
}

#[derive(Debug, Clone, DeriveDeserialize)]
struct TextValue {
    #[serde(rename = "$text")]
    value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_duration() {
        assert_eq!(parse_duration("PT30S"), Some(30.0));
        assert_eq!(parse_duration("PT1M30S"), Some(90.0));
        assert_eq!(parse_duration("PT1H2M3.5S"), Some(3723.5));
    }

    #[test]
    fn parses_segment_template_mpd() {
        let text = r#"<MPD type="static" mediaPresentationDuration="PT6S">
            <Period>
                <AdaptationSet>
                    <SegmentTemplate timescale="1" duration="2" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
                    <Representation id="v1" bandwidth="1000"/>
                </AdaptationSet>
            </Period>
        </MPD>"#;
        let mpd: Mpd = quick_xml::de::from_str(text).unwrap();
        assert_eq!(mpd.periods.len(), 1);
        assert_eq!(mpd.periods[0].adaptations[0].representations.len(), 1);
        assert_eq!(parse_duration(mpd.duration.as_deref().unwrap()), Some(6.0));
    }
}
