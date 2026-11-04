//! Small formatting helpers shared by the task list and detail drawer.

use crate::i18n;
use crate::model::{self, format_eta, format_pair, format_speed, Status};
use crate::TaskRow;

pub(crate) fn file_tone(name: &str) -> i32 {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.zst")
    {
        return 1;
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "iso" | "img" | "dmg" => 2,
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" => 1,
        "mkv" | "mp4" | "avi" | "mov" | "webm" => 3,
        "appimage" | "exe" | "deb" | "rpm" => 4,
        _ => 0,
    }
}

pub(crate) fn to_row(task: &model::Task, detail_id: i32) -> TaskRow {
    let status = task.status;
    let eta = match status {
        Status::Downloading => format_eta(task.remaining_seconds()),
        Status::Failed => i18n::tr("校验失败", "Checksum failed"),
        Status::Paused => i18n::tr("已暂停", "Paused"),
        Status::Queued => i18n::tr("等待开始", "Waiting to start"),
        _ => String::new(),
    };
    let kind_tone = file_tone(&task.name);
    TaskRow {
        id: task.id,
        name: task.name.clone().into(),
        source: task.source.clone().into(),
        kind_tone,
        progress: task.progress(),
        progress_text: format_pair(task.done, task.total).into(),
        speed: if status == Status::Downloading || status == Status::Seeding {
            format_speed(task.speed).into()
        } else {
            "—".into()
        },
        eta: eta.into(),
        status: status.code(),
        status_text: status.label().into(),
        paused: status == Status::Paused,
        completed: status == Status::Completed,
        failed: status == Status::Failed,
        selected: task.id == detail_id,
    }
}
