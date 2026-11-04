//! First-party plugin metadata rendered by the catalogue and drawer.

use std::path::PathBuf;

use aether_daemon::PluginInfo;

use crate::i18n;

pub(crate) fn plugin_blurb(info: &PluginInfo) -> String {
    match info.id.as_str() {
        "http" => i18n::tr(
            "HTTP / HTTPS 多线程下载",
            "Multi-threaded HTTP/HTTPS downloads",
        ),
        "bt" => i18n::tr(
            "BT / 磁力链下载与做种",
            "BitTorrent / magnet downloads and seeding",
        ),
        "ftp" => i18n::tr("FTP / FTPS 站点传输", "FTP / FTPS transfers"),
        "sftp" => i18n::tr("SFTP / SSH 文件传输", "SFTP / SSH file transfers"),
        "media" => i18n::tr(
            "网页音视频嗅探（HLS / DASH）",
            "Web video streams (HLS / DASH)",
        ),
        "extractor" => i18n::tr("yt-dlp 视频站点支持", "Video sites via yt-dlp"),
        "rtmp" => i18n::tr("RTMP 直播流录制", "RTMP live stream capture"),
        "webdav" => i18n::tr("WebDAV 远程目录", "Remote WebDAV directories"),
        "s3" => i18n::tr("S3 / 对象存储", "S3 / object storage"),
        "gdrive" => i18n::tr("Google Drive 分享链接", "Google Drive share links"),
        "onedrive" => i18n::tr("OneDrive 分享链接", "OneDrive share links"),
        "dropbox" => i18n::tr("Dropbox 分享链接", "Dropbox share links"),
        "baidupan" => i18n::tr("百度网盘（官方 API）", "Baidu Netdisk (official API)"),
        "aliyunpan" => i18n::tr("阿里云盘（官方 API）", "Aliyun Drive (official API)"),
        "thunder" => i18n::tr("迅雷链接", "Thunder links"),
        "ipfs" => i18n::tr("IPFS / IPNS 内容", "IPFS / IPNS content"),
        "aria2" => i18n::tr(
            "aria2 JSON-RPC 兼容（HTTP）",
            "aria2 JSON-RPC compatibility (HTTP)",
        ),
        other => {
            if info.protocols.is_empty() {
                i18n::tr(format!("协议：{other}"), format!("Protocol: {other}"))
            } else {
                i18n::tr(
                    format!("协议：{}", info.protocols.join(", ")),
                    format!("Protocols: {}", info.protocols.join(", ")),
                )
            }
        }
    }
}

/// Capability labels shown in the plugin catalogue. These are the manifest
/// contract, so they matter to users: "external process" means the plugin may
/// run yt-dlp/ffmpeg, and the daemon can refuse that class outright.
pub(crate) fn plugin_caps(info: &PluginInfo) -> String {
    if info.capabilities.is_empty() {
        return i18n::tr("无", "None");
    }
    info.capabilities
        .iter()
        .map(|capability| match capability.as_str() {
            "network" => i18n::tr("网络", "Network"),
            "filesystem:task" => i18n::tr("任务目录", "Task files"),
            "external-process" => i18n::tr("外部进程", "External process"),
            "clipboard" => i18n::tr("剪贴板", "Clipboard"),
            "http3" => "HTTP/3".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(crate) fn plugin_category(id: &str) -> String {
    match id {
        "http" => "HTTP".into(),
        "bt" => "BT".into(),
        "ftp" | "sftp" => i18n::tr("传输", "Transfer"),
        "media" | "extractor" | "rtmp" => i18n::tr("媒体", "Media"),
        "webdav" | "s3" | "ipfs" => i18n::tr("存储", "Storage"),
        "gdrive" | "onedrive" | "dropbox" | "baidupan" | "aliyunpan" | "thunder" => {
            i18n::tr("网盘", "Cloud drive")
        }
        "aria2" => "RPC".into(),
        _ => i18n::tr("其他", "Other"),
    }
}

pub(crate) fn plugin_icon(id: &str) -> i32 {
    match id {
        "http" => 0,                // link
        "bt" => 10,                 // seed
        "ftp" | "sftp" => 5,        // drive
        "media" | "extractor" => 1, // play
        "rtmp" => 11,               // signal
        "webdav" | "s3" | "gdrive" | "onedrive" | "dropbox" | "baidupan" | "aliyunpan" => 3, // cloud
        "thunder" | "aria2" => 11, // signal
        "ipfs" => 9,               // layers
        _ => 6,                    // list
    }
}

pub(crate) fn plugin_tone(id: &str) -> i32 {
    match id {
        "http" => 0,
        "bt" => 1,
        "ftp" | "sftp" => 2,
        "media" | "extractor" | "rtmp" => 3,
        "webdav" | "s3" | "ipfs" => 4,
        _ => 2,
    }
}

/// Standard native-messaging manifest locations; any `aether*.json` counts.
pub(crate) fn browser_extension_installed() -> bool {
    // Windows keeps the Chrome-family manifest path in HKCU; Firefox uses a
    // plain file under %APPDATA%.
    #[cfg(windows)]
    {
        let registry = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Google\Chrome\NativeMessagingHosts\com.pengtang1024.aether",
            ])
            .output();
        if registry.ok().is_some_and(|out| out.status.success()) {
            return true;
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let manifest = PathBuf::from(appdata)
                .join("Mozilla")
                .join("NativeMessagingHosts")
                .join("com.pengtang1024.aether.json");
            if manifest.is_file() {
                return true;
            }
        }
        return false;
    }
    #[cfg(not(windows))]
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    #[cfg(not(windows))]
    let home = PathBuf::from(home);
    #[cfg(not(windows))]
    {
        let dirs = [
            ".mozilla/native-messaging-hosts",
            ".config/google-chrome/NativeMessagingHosts",
            ".config/chromium/NativeMessagingHosts",
            ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        ];
        dirs.iter().any(|dir| {
            let dir = home.join(dir);
            dir.is_dir()
                && std::fs::read_dir(&dir).ok().is_some_and(|entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        let name = entry.file_name().to_string_lossy().to_lowercase();
                        name.starts_with("aether") && name.ends_with(".json")
                    })
                })
        })
    }
}
