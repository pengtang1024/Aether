//! Settings search index.
//!
//! Slint strings have no substring test, so the card/row identity and the
//! vocabulary live here: the page reports the query through
//! `settings-changed`, this module matches it against translated titles plus
//! a hand-written vocabulary, and returns both the matching cards and the
//! matching rows so the page can narrow a section down to the exact setting.
//!
//! A card whose own title/subtitle/keywords match shows every row; otherwise
//! only the rows whose title or keywords match are returned. Titles are
//! translated here as well, so the result list and the section headers cannot
//! drift apart (a unit test checks every id still exists in the .slint source).

use crate::i18n;

pub struct Card {
    pub id: &'static str,
    pub tab: i32,
    title: (&'static str, &'static str),
    subtitle: (&'static str, &'static str),
    /// Card-level terms only: row-specific words belong to `Row::keywords` so
    /// a query such as "token" opens the token row, not the whole card.
    keywords: &'static str,
}

/// Every filterable `SectionCard` in `ui/pages/settings.slint`, in tab order.
pub const CARDS: &[Card] = &[
    Card {
        id: "general-start",
        tab: 0,
        title: ("启动", "Startup"),
        subtitle: ("启动位置与窗口行为", "Launch position and window behaviour"),
        keywords: "启动 startup 窗口 window 托盘 tray 自启",
    },
    Card {
        id: "general-storage",
        tab: 0,
        title: ("存储", "Storage"),
        subtitle: ("下载目录", "Download folder"),
        keywords: "下载目录 保存路径 文件夹 目录 folder directory path download dir",
    },
    Card {
        id: "general-clipboard",
        tab: 0,
        title: ("剪贴板", "Clipboard"),
        subtitle: (
            "复制链接后提示一键下载",
            "Offer a download when a link is copied",
        ),
        keywords: "剪贴板 复制 链接 检测 clipboard copy link paste watch",
    },
    Card {
        id: "download-concurrency",
        tab: 1,
        title: ("并发与分段", "Concurrency"),
        subtitle: ("决定下载速度和服务器压力", "Download speed and server load"),
        keywords: "并发 concurrency 调度 服务器压力",
    },
    Card {
        id: "download-bandwidth",
        tab: 1,
        title: ("带宽", "Bandwidth"),
        subtitle: ("全局限速与智能调度", "Global speed limit and scheduling"),
        keywords: "带宽 bandwidth 速度 speed 调度",
    },
    Card {
        id: "download-after",
        tab: 1,
        title: ("任务", "Tasks"),
        subtitle: ("完成动作", "Actions after a task completes"),
        keywords: "完成后 打开文件夹 打开文件 完成动作 open folder file after done",
    },
    Card {
        id: "network-proxy",
        tab: 2,
        title: ("代理", "Proxy"),
        subtitle: (
            "让下载经过系统或自定义网络",
            "Route downloads through the system or a custom network",
        ),
        keywords: "代理 proxy 网络 network 隧道 翻墙",
    },
    Card {
        id: "network-protocol",
        tab: 2,
        title: ("协议与请求", "Protocol"),
        subtitle: ("兼容性与请求行为", "Compatibility and request behaviour"),
        keywords: "协议 protocol 请求 request 兼容 compatibility",
    },
    Card {
        id: "bt-connection",
        tab: 3,
        title: ("连接", "Connection"),
        subtitle: ("端口与节点发现", "Port and peer discovery"),
        keywords: "bt pt 连接 connection 节点 peer 磁力",
    },
    Card {
        id: "bt-seed",
        tab: 3,
        title: ("做种", "Seeding"),
        subtitle: ("上传带宽，0 为不限速", "Upload limit; 0 means unlimited"),
        keywords: "做种 seeding ratio 分享率",
    },
    Card {
        id: "bt-filter",
        tab: 3,
        title: ("过滤与 Tracker", "Filtering & trackers"),
        subtitle: ("屏蔽规则与私有 Tracker", "Blocklists and private trackers"),
        keywords: "屏蔽 blocklist tracker 私有",
    },
    Card {
        id: "media-capability",
        tab: 4,
        title: ("媒体下载", "Media"),
        subtitle: ("已实现的能力", "What is implemented"),
        keywords: "hls dash m3u8 mpd 媒体 视频 分段 解密 media stream",
    },
    Card {
        id: "media-ffmpeg",
        tab: 4,
        title: ("ffmpeg", "ffmpeg"),
        subtitle: (
            "rtmp 直播录制与媒体转封装",
            "RTMP capture and media remuxing",
        ),
        keywords: "ffmpeg 转封装 录制 rtmp transcode remux 路径",
    },
    Card {
        id: "plugin-catalog",
        tab: 5,
        title: ("插件库", "Plugin catalogue"),
        subtitle: ("已安装的插件与筛选", "Installed plugins and filters"),
        keywords: "插件 plugin 安装 卸载 更新 启用 回滚 权限 capability",
    },
    Card {
        id: "plugin-engine",
        tab: 5,
        title: ("插件引擎", "Plugin engine"),
        subtitle: (
            "插件目录、来源与更新",
            "Plugin directory, sources and updates",
        ),
        keywords: "插件 plugin 引擎 engine 来源 source 更新",
    },
    Card {
        id: "plugin-dev",
        tab: 5,
        title: ("开发", "Development"),
        subtitle: ("调试与重新加载", "Debugging and reloading"),
        keywords: "开发 调试 reload 重新加载 debug 日志 log",
    },
    Card {
        id: "schedule-plan",
        tab: 6,
        title: ("计划任务", "Schedule"),
        subtitle: (
            "时段外自动暂停队列，到点自动恢复",
            "Pause outside the window, resume on schedule",
        ),
        keywords: "计划 定时 schedule time 时段 暂停",
    },
    Card {
        id: "schedule-notify",
        tab: 6,
        title: ("通知", "Notifications"),
        subtitle: (
            "任务结束时的系统通知",
            "System notifications when a task ends",
        ),
        keywords: "通知 提醒 notification notify",
    },
    Card {
        id: "appearance-theme",
        tab: 7,
        title: ("主题", "Theme"),
        subtitle: ("主题、主色与动效", "Theme, accent and motion"),
        keywords: "外观 theme 配色",
    },
    Card {
        id: "appearance-ui",
        tab: 7,
        title: ("界面", "Interface"),
        subtitle: ("背景与动效", "Background and motion"),
        keywords: "界面 interface 外观 装饰",
    },
    Card {
        id: "advanced-paths",
        tab: 8,
        title: ("路径", "Paths"),
        subtitle: ("daemon 的真实数据位置", "Where the daemon keeps its data"),
        keywords: "路径 paths 数据 data 位置",
    },
    Card {
        id: "advanced-rpc",
        tab: 8,
        title: ("远程控制（RPC）", "Remote control (RPC)"),
        subtitle: (
            "CLI 与其他设备连接 daemon",
            "Let the CLI and other devices reach the daemon",
        ),
        keywords: "rpc 远程控制 remote control cli 6800",
    },
    Card {
        id: "advanced-reset",
        tab: 8,
        title: ("维护", "Maintenance"),
        subtitle: (
            "恢复默认外观与偏好",
            "Restore the default appearance and preferences",
        ),
        keywords: "重置 恢复默认 偏好 reset settings maintenance",
    },
    Card {
        id: "about-update",
        tab: 9,
        title: ("更新", "Updates"),
        subtitle: (
            "版本通道与自动更新",
            "Release channel and automatic updates",
        ),
        keywords: "更新 update 版本 version 通道",
    },
    Card {
        id: "about-support",
        tab: 9,
        title: ("支持", "Support"),
        subtitle: (
            "文档、社区、反馈与许可",
            "Docs, community, feedback and licensing",
        ),
        keywords: "支持 support 帮助 help",
    },
];

pub struct Row {
    pub id: &'static str,
    pub card: &'static str,
    title: (&'static str, &'static str),
    /// Row-specific terms (the card title is matched too, see `search`).
    keywords: &'static str,
}

/// Every `SettingRow`/`ValueSlider` with an id in the settings page.
pub const ROWS: &[Row] = &[
    Row {
        id: "general-start-page",
        card: "general-start",
        title: ("启动页面", "Start page"),
        keywords: "启动页面 主页 下载列表 上次页面 start page home last",
    },
    Row {
        id: "general-autostart",
        card: "general-start",
        title: ("开机自启", "Start at login"),
        keywords: "开机自启 自启动 登录 autostart login boot",
    },
    Row {
        id: "general-close-window",
        card: "general-start",
        title: ("关闭窗口", "Closing the window"),
        keywords: "关闭窗口 托盘 退出 close window tray quit",
    },
    Row {
        id: "general-download-dir",
        card: "general-storage",
        title: ("默认下载目录", "Default download folder"),
        keywords: "下载目录 保存路径 文件夹 folder directory path",
    },
    Row {
        id: "general-clipboard-watch",
        card: "general-clipboard",
        title: ("检测下载链接", "Detect download links"),
        keywords: "剪贴板 链接 检测 clipboard link watch copy",
    },
    Row {
        id: "download-max-tasks",
        card: "download-concurrency",
        title: ("同时进行的下载数", "Simultaneous downloads"),
        keywords: "同时下载 并发 队列 concurrent queue tasks",
    },
    Row {
        id: "download-connections",
        card: "download-concurrency",
        title: ("每任务连接数", "Connections per task"),
        keywords: "连接数 分段 每任务 connections segments",
    },
    Row {
        id: "download-retries",
        card: "download-concurrency",
        title: ("失败重试次数", "Retry count"),
        keywords: "重试 失败 retry",
    },
    Row {
        id: "download-limit-toggle",
        card: "download-bandwidth",
        title: ("启用全局限速", "Enable global speed limit"),
        keywords: "全局限速 开关 bandwidth limit",
    },
    Row {
        id: "download-limit",
        card: "download-bandwidth",
        title: ("下载限速", "Download limit"),
        keywords: "下载限速 带宽 速度 bandwidth speed limit kb",
    },
    Row {
        id: "download-after-done",
        card: "download-after",
        title: ("完成后动作", "After completion"),
        keywords: "完成后 打开文件夹 打开文件 after done open",
    },
    Row {
        id: "network-proxy-mode",
        card: "network-proxy",
        title: ("代理模式", "Proxy mode"),
        keywords: "代理模式 跟随系统 直连 手动 pac proxy mode system direct manual",
    },
    Row {
        id: "network-proxy-url",
        card: "network-proxy",
        title: ("代理地址", "Proxy address"),
        keywords: "代理地址 socks5 http https proxy url address",
    },
    Row {
        id: "network-dns",
        card: "network-proxy",
        title: ("DNS 服务器", "DNS servers"),
        keywords: "dns 服务器 解析 resolver",
    },
    Row {
        id: "network-ip-mode",
        card: "network-proxy",
        title: ("IP 协议", "IP protocol"),
        keywords: "ip 协议 ipv4 ipv6 protocol",
    },
    Row {
        id: "network-user-agent",
        card: "network-protocol",
        title: ("User-Agent", "User-Agent"),
        keywords: "user-agent ua 请求头 header",
    },
    Row {
        id: "network-headers",
        card: "network-protocol",
        title: ("自定义请求头", "Custom headers"),
        keywords: "请求头 自定义 header referer cookie",
    },
    Row {
        id: "network-ftp-passive",
        card: "network-protocol",
        title: ("FTP 被动模式", "FTP passive mode"),
        keywords: "ftp 被动 主动 passive active",
    },
    Row {
        id: "bt-port",
        card: "bt-connection",
        title: ("监听端口", "Listen port"),
        keywords: "监听端口 port 转发 listen",
    },
    Row {
        id: "bt-dht",
        card: "bt-connection",
        title: ("DHT", "DHT"),
        keywords: "dht 节点发现 peer discovery",
    },
    Row {
        id: "bt-upload-limit",
        card: "bt-seed",
        title: ("上传限速", "Upload limit"),
        keywords: "上传 限速 做种 upload limit seeding",
    },
    Row {
        id: "bt-blocklist",
        card: "bt-filter",
        title: ("屏蔽列表路径", "Blocklist path"),
        keywords: "屏蔽 黑名单 blocklist dat 过滤",
    },
    Row {
        id: "bt-trackers",
        card: "bt-filter",
        title: ("Tracker 列表", "Tracker list"),
        keywords: "tracker 列表 announce 私有",
    },
    Row {
        id: "media-hls",
        card: "media-capability",
        title: ("HLS / DASH 直链", "Direct HLS / DASH links"),
        keywords: "hls dash m3u8 mpd 直播 媒体 stream",
    },
    Row {
        id: "media-ffmpeg-path",
        card: "media-ffmpeg",
        title: ("ffmpeg 路径", "ffmpeg path"),
        keywords: "ffmpeg 路径 转封装 rtmp remux path",
    },
    Row {
        id: "plugin-auto-update",
        card: "plugin-engine",
        title: ("自动更新插件", "Auto-update plugins"),
        keywords: "插件 自动更新 auto update",
    },
    Row {
        id: "plugin-external-process",
        card: "plugin-engine",
        title: ("允许外部进程", "Allow external processes"),
        keywords: "外部进程 external process yt-dlp ffmpeg",
    },
    Row {
        id: "plugin-pins",
        card: "plugin-engine",
        title: ("固定插件版本", "Pin plugin versions"),
        keywords: "固定版本 pin 插件版本",
    },
    Row {
        id: "plugin-dir",
        card: "plugin-engine",
        title: ("插件目录", "Plugin directory"),
        keywords: "插件目录 安装位置 directory plugins",
    },
    Row {
        id: "plugin-install",
        card: "plugin-engine",
        title: ("安装插件", "Install plugin"),
        keywords: "安装插件 插件包 install package",
    },
    Row {
        id: "plugin-reload",
        card: "plugin-dev",
        title: ("重新加载插件", "Reload plugins"),
        keywords: "重新加载 插件 reload",
    },
    Row {
        id: "schedule-enabled",
        card: "schedule-plan",
        title: ("启用计划下载", "Enable scheduled downloads"),
        keywords: "计划 定时 开关 schedule enable",
    },
    Row {
        id: "schedule-start",
        card: "schedule-plan",
        title: ("开始时间", "Start time"),
        keywords: "开始时间 计划 start time",
    },
    Row {
        id: "schedule-end",
        card: "schedule-plan",
        title: ("结束时间", "End time"),
        keywords: "结束时间 跨午夜 end time",
    },
    Row {
        id: "schedule-notify-done",
        card: "schedule-notify",
        title: ("下载完成", "Download completed"),
        keywords: "完成 通知 提醒 done notification",
    },
    Row {
        id: "schedule-notify-fail",
        card: "schedule-notify",
        title: ("下载失败", "Download failed"),
        keywords: "失败 通知 提醒 fail notification",
    },
    Row {
        id: "appearance-language",
        card: "appearance-theme",
        title: ("界面语言", "Interface language"),
        keywords: "界面语言 语言 english 中文 language",
    },
    Row {
        id: "appearance-theme-mode",
        card: "appearance-theme",
        title: ("主题", "Theme"),
        keywords: "主题 深色 浅色 跟随系统 theme dark light system",
    },
    Row {
        id: "appearance-accent",
        card: "appearance-theme",
        title: ("主色", "Accent color"),
        keywords: "主色 强调色 配色 accent color",
    },
    Row {
        id: "appearance-flow",
        card: "appearance-ui",
        title: ("背景流线", "Background flow"),
        keywords: "背景 流线 装饰 flow background",
    },
    Row {
        id: "appearance-motion",
        card: "appearance-ui",
        title: ("界面动效", "Interface motion"),
        keywords: "动效 动画 无障碍 motion animation",
    },
    Row {
        id: "advanced-data-dir",
        card: "advanced-paths",
        title: ("数据库路径", "Database path"),
        keywords: "数据库 路径 数据目录 database data dir sqlite",
    },
    Row {
        id: "advanced-plugins-dir",
        card: "advanced-paths",
        title: ("插件目录", "Plugin directory"),
        keywords: "插件目录 路径 plugins dir",
    },
    Row {
        id: "advanced-rpc-listen",
        card: "advanced-rpc",
        title: ("监听地址", "Listen address"),
        keywords: "监听地址 rpc 端口 6800 listen address",
    },
    Row {
        id: "advanced-rpc-secret",
        card: "advanced-rpc",
        title: ("访问令牌", "Access token"),
        keywords: "访问令牌 token 密码 secret",
    },
    Row {
        id: "advanced-rpc-status",
        card: "advanced-rpc",
        title: ("状态", "Status"),
        keywords: "状态 status 连接",
    },
    Row {
        id: "advanced-reset-prefs",
        card: "advanced-reset",
        title: ("重置界面偏好", "Reset UI preferences"),
        keywords: "重置 恢复默认 偏好 reset prefs",
    },
    Row {
        id: "about-update-source",
        card: "about-update",
        title: ("更新源", "Update source"),
        keywords: "更新源 update source channel",
    },
    Row {
        id: "about-auto-update",
        card: "about-update",
        title: ("自动下载更新", "Download updates automatically"),
        keywords: "自动下载更新 auto update download",
    },
    Row {
        id: "about-changelog",
        card: "about-update",
        title: ("版本说明", "Release notes"),
        keywords: "changelog release notes 版本说明",
    },
    Row {
        id: "about-docs",
        card: "about-support",
        title: ("文档", "Documents"),
        keywords: "文档 docs 帮助 help",
    },
    Row {
        id: "about-community",
        card: "about-support",
        title: ("社区", "Community"),
        keywords: "社区 community 讨论",
    },
    Row {
        id: "about-feedback",
        card: "about-support",
        title: ("反馈问题", "Report an issue"),
        keywords: "反馈 问题 bug feedback issue",
    },
    Row {
        id: "about-license",
        card: "about-support",
        title: ("许可", "License"),
        keywords: "许可 license mit slint",
    },
];

pub struct Hit {
    pub tab: i32,
    pub id: &'static str,
    /// Title split around the matched term; the middle part is highlighted.
    pub title_before: String,
    pub title_hit: String,
    pub title_after: String,
    pub subtitle_before: String,
    pub subtitle_hit: String,
    pub subtitle_after: String,
    pub section: String,
    /// "Network · 2" on the first hit of a section, empty for the rest.
    pub heading: String,
}

pub struct Results {
    pub hits: Vec<Hit>,
    /// "|id|id|" card ids; empty means "everything visible".
    pub cards: String,
    /// "|id|id|" row ids for the focused section.
    pub rows: String,
}

/// Category name for a tab index, used by the result rows.
pub fn section_name(tab: i32) -> String {
    match tab {
        0 => i18n::tr("常规", "General"),
        1 => i18n::tr("下载", "Downloads"),
        2 => i18n::tr("网络", "Network"),
        3 => "BT / PT".to_string(),
        4 => i18n::tr("媒体与浏览器", "Media & browser"),
        5 => i18n::tr("插件", "Plugins"),
        6 => i18n::tr("计划与通知", "Schedule & notifications"),
        7 => i18n::tr("外观", "Appearance"),
        8 => i18n::tr("高级", "Advanced"),
        _ => i18n::tr("关于与支持", "About & support"),
    }
}

fn matches(haystack: &str, tokens: &[String]) -> bool {
    let haystack = haystack.to_lowercase();
    tokens.iter().all(|token| haystack.contains(token.as_str()))
}

/// Split `text` into (before, match, after) around the first token that
/// occurs in it. `to_lowercase` must not change the byte length for the
/// indices to map back, so text that does (rare) is returned unsplit.
fn highlight(text: &str, tokens: &[String]) -> (String, String, String) {
    let lower = text.to_lowercase();
    if lower.len() == text.len() {
        for token in tokens {
            if let Some(pos) = lower.find(token.as_str()) {
                let end = pos + token.len();
                if text.is_char_boundary(pos) && text.is_char_boundary(end) {
                    return (
                        text[..pos].to_string(),
                        text[pos..end].to_string(),
                        text[end..].to_string(),
                    );
                }
            }
        }
    }
    (text.to_string(), String::new(), String::new())
}

fn make_hit(tab: i32, id: &'static str, title: &str, subtitle: &str, tokens: &[String]) -> Hit {
    let (title_before, title_hit, title_after) = highlight(title, tokens);
    let (subtitle_before, subtitle_hit, subtitle_after) = highlight(subtitle, tokens);
    Hit {
        tab,
        id,
        title_before,
        title_hit,
        title_after,
        subtitle_before,
        subtitle_hit,
        subtitle_after,
        section: section_name(tab),
        heading: String::new(),
    }
}

fn membership(ids: Vec<&'static str>) -> String {
    let mut out = String::new();
    for id in ids {
        out.push('|');
        out.push_str(id);
    }
    if !out.is_empty() {
        out.push('|');
    }
    out
}

/// Every whitespace-separated token must appear in the vocabulary. Titles are
/// matched in both languages, so an English query works in any interface
/// language.
pub fn search(query: &str) -> Results {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .collect();
    let mut hits: Vec<Hit> = Vec::new();
    let mut cards: Vec<&'static str> = Vec::new();
    let mut rows: Vec<&'static str> = Vec::new();
    if tokens.is_empty() {
        return Results {
            hits,
            cards: String::new(),
            rows: String::new(),
        };
    }

    for card in CARDS {
        let card_haystack = format!(
            "{} {} {} {} {}",
            card.title.0, card.title.1, card.subtitle.0, card.subtitle.1, card.keywords
        );
        let card_rows: Vec<&Row> = ROWS.iter().filter(|row| row.card == card.id).collect();

        if matches(&card_haystack, &tokens) {
            cards.push(card.id);
            for row in &card_rows {
                rows.push(row.id);
            }
            let title = i18n::tr(card.title.0, card.title.1);
            let subtitle = i18n::tr(card.subtitle.0, card.subtitle.1);
            hits.push(make_hit(card.tab, card.id, &title, &subtitle, &tokens));
            continue;
        }

        let mut card_matched = false;
        for row in card_rows {
            let row_haystack = format!(
                "{} {} {} {}",
                row.title.0, row.title.1, row.keywords, card.title.0
            );
            if matches(&row_haystack, &tokens) {
                card_matched = true;
                rows.push(row.id);
                let title = i18n::tr(row.title.0, row.title.1);
                let subtitle = i18n::tr(card.title.0, card.title.1);
                hits.push(make_hit(card.tab, row.id, &title, &subtitle, &tokens));
            }
        }
        if card_matched {
            cards.push(card.id);
        }
    }

    // Group headings: "Section · N" above the first hit of each section.
    let mut counts: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    for hit in &hits {
        *counts.entry(hit.tab).or_default() += 1;
    }
    let mut last_tab = -1;
    for hit in &mut hits {
        if hit.tab != last_tab {
            let count = counts.get(&hit.tab).copied().unwrap_or(0);
            hit.heading = format!("{} · {}", hit.section, count);
            last_tab = hit.tab;
        }
    }

    Results {
        hits,
        cards: membership(cards),
        rows: membership(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(query: &str) -> Vec<&'static str> {
        search(query).hits.into_iter().map(|hit| hit.id).collect()
    }

    /// The whole settings page source: the page file plus every component it
    /// was split into under `ui/pages/settings/`.
    fn ui_source() -> String {
        let mut source = String::from(include_str!("../ui/pages/settings.slint"));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/pages/settings");
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("settings component directory")
            .map(|entry| entry.expect("readable entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "slint"))
            .collect();
        entries.sort();
        for path in entries {
            source.push_str(&std::fs::read_to_string(&path).expect("readable slint file"));
        }
        source
    }

    #[test]
    fn card_ids_are_unique_and_present_in_the_ui() {
        let source = ui_source();
        let mut seen = std::collections::HashSet::new();
        for card in CARDS {
            assert!(seen.insert(card.id), "duplicate card id {}", card.id);
            assert!(
                source.contains(&format!("\n            id: \"{}\"", card.id)),
                "{} is not a SectionCard id in the settings page",
                card.id
            );
        }
        assert_eq!(source.matches("\n            id: \"").count(), CARDS.len());
    }

    #[test]
    fn row_ids_are_unique_and_present_in_the_ui() {
        let source = ui_source();
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            assert!(seen.insert(row.id), "duplicate row id {}", row.id);
            assert!(
                CARDS.iter().any(|card| card.id == row.card),
                "{} points at an unknown card {}",
                row.id,
                row.card
            );
            assert!(
                source.contains(&format!("\n                id: \"{}\"", row.id)),
                "{} is not a SettingRow id in settings.slint",
                row.id
            );
        }
        assert_eq!(
            source.matches("\n                id: \"").count(),
            ROWS.len()
        );
    }

    #[test]
    fn matches_titles_in_both_languages() {
        assert_eq!(ids("代理"), vec!["network-proxy"]);
        assert_eq!(ids("proxy"), vec!["network-proxy"]);
        // "ffmpeg" is also mentioned by the external-process row.
        assert_eq!(
            ids("FFMPEG"),
            vec!["media-ffmpeg", "plugin-external-process"]
        );
    }

    #[test]
    fn matches_specific_rows_not_whole_cards() {
        assert_eq!(ids("令牌"), vec!["advanced-rpc-secret"]);
        assert_eq!(ids("socks5"), vec!["network-proxy-url"]);
        assert_eq!(ids("数据库"), vec!["advanced-data-dir"]);
        // BT's subtitle is literally about ports, so the whole card matches.
        assert_eq!(ids("端口"), vec!["bt-connection", "advanced-rpc-listen"]);
    }

    #[test]
    fn requires_every_token_but_any_order() {
        assert_eq!(ids("代理 pac"), vec!["network-proxy-mode"]);
        assert_eq!(ids("pac 代理"), vec!["network-proxy-mode"]);
        assert!(ids("代理 ffmpeg").is_empty());
    }

    #[test]
    fn whole_card_match_reveals_all_its_rows() {
        let results = search("代理");
        assert_eq!(results.cards, "|network-proxy|");
        for row in ROWS.iter().filter(|row| row.card == "network-proxy") {
            assert!(
                results.rows.contains(&format!("|{}|", row.id)),
                "{} missing from {}",
                row.id,
                results.rows
            );
        }
    }

    #[test]
    fn empty_query_has_no_hits() {
        assert!(search("").hits.is_empty());
        assert!(search("   ").hits.is_empty());
    }

    #[test]
    fn highlight_splits_the_matched_term() {
        let tokens = vec!["token".to_string()];
        let (before, hit, after) = highlight("Access token list", &tokens);
        assert_eq!(
            (before.as_str(), hit.as_str(), after.as_str()),
            ("Access ", "token", " list")
        );
        let (before, hit, _) = highlight("访问令牌", &["令牌".to_string()]);
        assert_eq!((before.as_str(), hit.as_str()), ("访问", "令牌"));
    }
}
