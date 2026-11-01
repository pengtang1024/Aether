//! Settings search index.
//!
//! Slint strings have no substring test, so the card identity and vocabulary
//! live here: the page reports the query through `search-changed`, this module
//! matches it against translated titles plus a hand-written vocabulary, and
//! the page filters its cards with the returned ids. Card titles are also
//! translated here, so the result list and the section headers cannot drift
//! apart (a unit test checks every id still exists in the .slint source).

use crate::i18n;

pub struct Card {
    pub id: &'static str,
    pub tab: i32,
    title: (&'static str, &'static str),
    subtitle: (&'static str, &'static str),
    /// Extra search terms: synonyms, English names, protocol names. Kept
    /// separate from the UI copy so a wording change does not break search.
    keywords: &'static str,
}

/// Every filterable `SectionCard` in `ui/pages/settings.slint`, in tab order.
pub const CARDS: &[Card] = &[
    Card {
        id: "general-start",
        tab: 0,
        title: ("启动", "Startup"),
        subtitle: ("启动位置与窗口行为", "Launch position and window behaviour"),
        keywords: "开机自启 自启动 启动页面 上次页面 主页 关闭窗口 托盘 autostart startup tray close window",
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
        subtitle: ("复制链接后提示一键下载", "Offer a download when a link is copied"),
        keywords: "剪贴板 复制 链接 检测 clipboard copy link paste watch",
    },
    Card {
        id: "download-concurrency",
        tab: 1,
        title: ("并发与分段", "Concurrency"),
        subtitle: ("决定下载速度和服务器压力", "Download speed and server load"),
        keywords: "同时下载 并发 连接数 重试 等待队列 分段 concurrent connections retry queue segments",
    },
    Card {
        id: "download-bandwidth",
        tab: 1,
        title: ("带宽", "Bandwidth"),
        subtitle: ("全局限速与智能调度", "Global speed limit and scheduling"),
        keywords: "限速 带宽 速度 每秒 kb/s bandwidth speed limit throttle",
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
        subtitle: ("让下载经过系统或自定义网络", "Route downloads through the system or a custom network"),
        keywords: "代理 proxy socks5 http https pac 直连 跟随系统 代理地址 端口 隧道 翻墙",
    },
    Card {
        id: "network-protocol",
        tab: 2,
        title: ("协议与请求", "Protocol"),
        subtitle: ("兼容性与请求行为", "Compatibility and request behaviour"),
        keywords: "user-agent ua 请求头 header referer cookie dns ipv4 ipv6 ftp 被动 passive",
    },
    Card {
        id: "bt-connection",
        tab: 3,
        title: ("连接", "Connection"),
        subtitle: ("端口与节点发现", "Port and peer discovery"),
        keywords: "bt pt 端口 port dht 节点 peer magnet 磁力 监听 listen",
    },
    Card {
        id: "bt-seed",
        tab: 3,
        title: ("做种", "Seeding"),
        subtitle: ("上传带宽，0 为不限速", "Upload limit; 0 means unlimited"),
        keywords: "做种 上传 限速 seeding upload ratio 分享率",
    },
    Card {
        id: "bt-filter",
        tab: 3,
        title: ("过滤与 Tracker", "Filtering & trackers"),
        subtitle: ("屏蔽规则与私有 Tracker", "Blocklists and private trackers"),
        keywords: "屏蔽 黑名单 blocklist tracker 过滤 dat 私有",
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
        subtitle: ("rtmp 直播录制与媒体转封装", "RTMP capture and media remuxing"),
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
        subtitle: ("插件目录、来源与更新", "Plugin directory, sources and updates"),
        keywords: "插件目录 自动更新 外部进程 external process 固定版本 pin 安装 install",
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
        subtitle: ("时段外自动暂停队列，到点自动恢复", "Pause outside the window, resume on schedule"),
        keywords: "计划 定时 开始 结束 跨午夜 暂停 schedule time window",
    },
    Card {
        id: "schedule-notify",
        tab: 6,
        title: ("通知", "Notifications"),
        subtitle: ("任务结束时的系统通知", "System notifications when a task ends"),
        keywords: "通知 提醒 完成 失败 notification notify done fail",
    },
    Card {
        id: "appearance-theme",
        tab: 7,
        title: ("主题", "Theme"),
        subtitle: ("主题、主色与动效", "Theme, accent and motion"),
        keywords: "语言 主题 深色 浅色 跟随系统 主色 强调色 language theme dark light accent",
    },
    Card {
        id: "appearance-ui",
        tab: 7,
        title: ("界面", "Interface"),
        subtitle: ("背景与动效", "Background and motion"),
        keywords: "背景 流线 动效 动画 animation motion background flow",
    },
    Card {
        id: "advanced-paths",
        tab: 8,
        title: ("路径", "Paths"),
        subtitle: ("daemon 的真实数据位置", "Where the daemon keeps its data"),
        keywords: "数据库 路径 数据目录 data dir database plugins dir 文件",
    },
    Card {
        id: "advanced-rpc",
        tab: 8,
        title: ("远程控制（RPC）", "Remote control (RPC)"),
        subtitle: ("CLI 与其他设备连接 daemon", "Let the CLI and other devices reach the daemon"),
        keywords: "rpc 远程控制 监听 地址 令牌 token 密码 secret aria2 6800 cli",
    },
    Card {
        id: "advanced-reset",
        tab: 8,
        title: ("维护", "Maintenance"),
        subtitle: ("恢复默认外观与偏好", "Restore the default appearance and preferences"),
        keywords: "重置 恢复默认 偏好 reset settings maintenance",
    },
    Card {
        id: "about-update",
        tab: 9,
        title: ("更新", "Updates"),
        subtitle: ("版本通道与自动更新", "Release channel and automatic updates"),
        keywords: "更新 版本 自动下载 update version changelog 版本说明",
    },
    Card {
        id: "about-support",
        tab: 9,
        title: ("支持", "Support"),
        subtitle: ("文档、社区、反馈与许可", "Docs, community, feedback and licensing"),
        keywords: "文档 社区 反馈 许可 license docs community feedback slint mit",
    },
];

pub struct Hit {
    pub tab: i32,
    pub id: &'static str,
    pub title: String,
    pub subtitle: String,
    pub section: String,
    /// "Network · 2" on the first hit of a section, empty for the rest.
    pub heading: String,
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

/// Every whitespace-separated token must appear in the card's vocabulary.
/// Titles are matched in both languages so an English query works regardless
/// of the interface language.
pub fn search(query: &str) -> Vec<Hit> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = CARDS
        .iter()
        .filter(|card| {
            let haystack = format!(
                "{} {} {} {} {}",
                card.title.0, card.title.1, card.subtitle.0, card.subtitle.1, card.keywords
            )
            .to_lowercase();
            tokens.iter().all(|token| haystack.contains(token.as_str()))
        })
        .map(|card| Hit {
            tab: card.tab,
            id: card.id,
            title: i18n::tr(card.title.0, card.title.1),
            subtitle: i18n::tr(card.subtitle.0, card.subtitle.1),
            section: section_name(card.tab),
            heading: String::new(),
        })
        .collect::<Vec<_>>();

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
    hits
}

/// `|id|id|` membership string consumed by `SettingsFilter.shows`.
pub fn hit_ids(hits: &[Hit]) -> String {
    let mut ids = String::new();
    for hit in hits {
        ids.push('|');
        ids.push_str(hit.id);
    }
    if !ids.is_empty() {
        ids.push('|');
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(query: &str) -> Vec<&'static str> {
        search(query).into_iter().map(|hit| hit.id).collect()
    }

    #[test]
    fn card_ids_are_unique_and_present_in_the_ui() {
        let source = include_str!("../ui/pages/settings.slint");
        let mut seen = std::collections::HashSet::new();
        for card in CARDS {
            assert!(seen.insert(card.id), "duplicate id {}", card.id);
            assert!(
                source.contains(&format!("id: \"{}\"", card.id)),
                "{} is not a SectionCard id in settings.slint",
                card.id
            );
        }
        assert_eq!(source.matches("\n            id: \"").count(), CARDS.len());
    }

    #[test]
    fn matches_titles_in_both_languages() {
        assert_eq!(ids("代理"), vec!["network-proxy"]);
        assert_eq!(ids("proxy"), vec!["network-proxy"]);
        assert_eq!(ids("FFMPEG"), vec!["media-ffmpeg"]);
    }

    #[test]
    fn matches_vocabulary_not_just_titles() {
        assert_eq!(ids("令牌"), vec!["advanced-rpc"]);
        assert_eq!(ids("socks5"), vec!["network-proxy"]);
        assert_eq!(ids("限速"), vec!["download-bandwidth", "bt-seed"]);
    }

    #[test]
    fn requires_every_token_but_any_order() {
        assert_eq!(ids("代理 pac"), vec!["network-proxy"]);
        assert_eq!(ids("pac 代理"), vec!["network-proxy"]);
        assert!(ids("代理 ffmpeg").is_empty());
    }

    #[test]
    fn empty_query_has_no_hits() {
        assert!(search("").is_empty());
        assert!(search("   ").is_empty());
    }

    #[test]
    fn membership_string_is_bracketed() {
        let hits = search("代理");
        assert_eq!(hit_ids(&hits), "|network-proxy|");
        assert!(hit_ids(&[]).is_empty());
    }
}
