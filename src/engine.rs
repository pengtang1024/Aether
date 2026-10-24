//! The simulated download engine.
//!
//! Every tick it advances the active tasks, recomputes live speeds and keeps a
//! one-minute speed history per task. The UI never mutates tasks directly — it
//! sends intents here and re-renders from the resulting state.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{format_size, Status, Task, CHUNK_COUNT, HISTORY_LEN};

const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * MB;

pub struct Peer {
    pub address: String,
    pub speed: f64,
    pub ratio: f32,
    pub kind: i32,
}

pub struct LogEntry {
    pub time: String,
    pub level: &'static str,
    pub message: String,
}

pub struct Engine {
    tasks: Vec<Task>,
    rng: u64,
    next_id: i32,
    /// Seconds accumulated since the last history sample.
    sample_accum: f32,
    /// Monotonic time used to drive the ambient background.
    phase: f32,
}

impl Engine {
    pub fn new() -> Self {
        let mut engine = Engine {
            tasks: Vec::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
            next_id: 1,
            sample_accum: 0.0,
            phase: 0.0,
        };
        engine.seed_demo_tasks();
        engine
    }

    // -- queries -----------------------------------------------------------

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn task(&self, id: i32) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn active_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == Status::Downloading)
            .count()
    }

    pub fn completed_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == Status::Completed)
            .count()
    }

    pub fn total_speed(&self) -> f64 {
        self.tasks
            .iter()
            .filter(|t| t.status == Status::Downloading)
            .map(|t| t.speed)
            .sum()
    }

    pub fn drift(&self) -> f32 {
        (self.phase * 0.22).sin()
    }

    // -- intents -----------------------------------------------------------

    pub fn tick(&mut self, dt: f32) {
        self.phase += dt;
        self.sample_accum += dt;
        let sample_due = self.sample_accum >= 1.0;
        if sample_due {
            self.sample_accum -= 1.0;
        }

        let rng = &mut self.rng;
        for task in &mut self.tasks {
            match task.status {
                Status::Downloading => {
                    // ±18 % jitter keeps the speed read-out alive.
                    let jitter = (0.82 + rand_f32(rng) * 0.36) as f64;
                    task.speed = task.nominal_speed * jitter;
                    task.done += task.speed * dt as f64;
                    if task.done >= task.total as f64 {
                        task.done = task.total as f64;
                        task.speed = 0.0;
                        task.status = if task.torrent {
                            Status::Seeding
                        } else {
                            Status::Completed
                        };
                    }
                }
                Status::Seeding => {
                    // Uploading is slower than the original download, but not
                    // a rounding error: a few MB/s keeps the read-out honest.
                    task.speed = (0.6 + rand_f32(rng) as f64 * 2.2) * MB;
                }
                _ => task.speed = 0.0,
            }

            // Only running tasks extend the history: finished or paused tasks
            // keep the curve of their last active minute.
            if sample_due && task.status.is_running() {
                task.push_sample(task.speed);
            }
        }
    }

    /// 0 = pause/resume, 1 = retry, 2 = remove, 3 = reveal in folder.
    pub fn apply_action(&mut self, id: i32, action: i32) {
        match action {
            0 => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                    match task.status {
                        Status::Downloading | Status::Seeding => {
                            task.status = Status::Paused;
                            task.speed = 0.0;
                        }
                        Status::Paused => {
                            task.status = if task.progress() >= 1.0 && task.torrent {
                                Status::Seeding
                            } else {
                                Status::Downloading
                            };
                            task.speed = task.nominal_speed;
                        }
                        Status::Queued => {
                            task.status = Status::Downloading;
                            task.speed = task.nominal_speed;
                        }
                        _ => {}
                    }
                }
            }
            1 => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                    if task.status == Status::Failed {
                        task.status = Status::Downloading;
                        task.done = 0.0;
                        task.speed = task.nominal_speed;
                        task.history.clear();
                        task.history.push_back(0.0);
                    }
                }
            }
            2 => {
                self.tasks.retain(|t| t.id != id);
            }
            3 => {
                // Revealing the file is a UI affordance; the mockup shows a
                // snackbar instead of touching the desktop.
            }
            _ => {}
        }
    }

    pub fn add_task(&mut self, url: &str, path: &str, speed_limit_kb: f32, note: &str) -> i32 {
        let name = file_name_from_url(url);
        let total = (180.0 + rand_f32(&mut self.rng) as f64 * 1400.0) * MB;
        let mut nominal = (6.0 + rand_f32(&mut self.rng) as f64 * 14.0) * MB;
        if speed_limit_kb > 0.0 {
            nominal = nominal.min(speed_limit_kb as f64 * 1024.0);
        }
        let source = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .filter(|host| !host.is_empty())
            .unwrap_or("直接链接")
            .to_string();

        let id = self.next_id;
        self.next_id += 1;

        let order = shuffled_order(&mut self.rng);
        let mut task = Task {
            id,
            name,
            source,
            path: path.to_string(),
            note: note.to_string(),
            total: total as u64,
            done: 0.0,
            status: Status::Downloading,
            torrent: false,
            nominal_speed: nominal,
            speed: nominal,
            history: VecDeque::new(),
            order,
        };
        for _ in 0..HISTORY_LEN {
            task.push_sample(0.0);
        }
        self.tasks.insert(0, task);
        id
    }

    pub fn peers(&self, id: i32) -> Vec<Peer> {
        let Some(task) = self.task(id) else {
            return Vec::new();
        };
        if !task.torrent {
            return Vec::new();
        }
        // Deterministic per task so the list does not reshuffle on every tick.
        let mut rng = 0xC0FF_EE12_3456_789A ^ (id as u64).wrapping_mul(0x9E37_79B9);
        let count = 6 + (id as usize % 4);
        (0..count)
            .map(|i| {
                let a = 203;
                let b = rand_u32(&mut rng) % 2;
                let c = rand_u32(&mut rng) % 256;
                let d = 1 + rand_u32(&mut rng) % 254;
                let port = 51413 + i as u32 * 7;
                Peer {
                    address: format!("{a}.{b}.{c}.{d}:{port}"),
                    speed: (0.4 + rand_f32(&mut rng) as f64 * 2.6) * MB,
                    ratio: (task.progress() * (0.55 + rand_f32(&mut rng) * 0.45)).clamp(0.0, 1.0),
                    kind: i32::from(i % 4 == 0),
                }
            })
            .collect()
    }

    pub fn logs(&self, id: i32) -> Vec<LogEntry> {
        let Some(task) = self.task(id) else {
            return Vec::new();
        };
        let base = now_seconds();
        let seed = 7 + (id as u64 % 5);
        let mut lines = Vec::new();
        let progress = task.progress();

        lines.push(log_line(
            base - 4,
            "INF",
            format!("已连接 {} 个节点，正在接收数据", 6 + id % 4),
        ));
        lines.push(log_line(
            base - 11,
            "INF",
            format!(
                "分片校验通过 {}/{}",
                (progress * CHUNK_COUNT as f32) as u32,
                CHUNK_COUNT
            ),
        ));
        if task.status == Status::Downloading {
            lines.push(log_line(
                base - 19,
                "INF",
                format!("平均速度 {}，缓冲区正常", format_size(task.speed.max(1.0))),
            ));
        }
        if task.status == Status::Paused {
            lines.push(log_line(
                base - 24,
                "WRN",
                "用户暂停了任务，连接已保留".to_string(),
            ));
        }
        if task.status == Status::Failed {
            lines.push(log_line(
                base - 26,
                "ERR",
                "分片 SHA-1 校验失败，已停止写入".to_string(),
            ));
            lines.push(log_line(
                base - 27,
                "WRN",
                "正在重试镜像源 mirrors.edge.example".to_string(),
            ));
        }
        if task.status == Status::Seeding {
            lines.push(log_line(
                base - 31,
                "INF",
                "下载完成，进入做种状态".to_string(),
            ));
        }
        lines.push(log_line(
            base - 38,
            "INF",
            format!("开始任务，保存到 {}", shorten_path(&task.path)),
        ));
        lines.push(log_line(
            base - 40,
            "INF",
            format!("创建 {} 个连接线程", seed),
        ));
        lines.push(log_line(base - 41, "INF", "读取任务元数据完成".to_string()));
        lines
    }

    // -- demo data ---------------------------------------------------------

    fn seed_demo_tasks(&mut self) {
        struct Seed {
            name: &'static str,
            source: &'static str,
            total: f64,
            progress: f32,
            speed_mb: f64,
            status: Status,
            torrent: bool,
            note: &'static str,
        }

        let seeds = [
            Seed {
                name: "ubuntu-24.04.3-desktop-amd64.iso",
                source: "releases.ubuntu.com",
                total: 6.0 * GB,
                progress: 0.713,
                speed_mb: 18.6,
                status: Status::Downloading,
                torrent: false,
                note: "工作机重装用",
            },
            Seed {
                name: "Sintel.2010.1080p.BluRay.x264.mkv",
                source: "linuxtracker.org",
                total: 8.2 * GB,
                progress: 0.452,
                speed_mb: 6.2,
                status: Status::Downloading,
                torrent: true,
                note: "CC 开源电影",
            },
            Seed {
                name: "blender-4.4.0-linux-x64.tar.xz",
                source: "mirrors.aliyun.com",
                total: 328.0 * MB,
                progress: 1.0,
                speed_mb: 0.0,
                status: Status::Completed,
                torrent: false,
                note: "3D 素材制作",
            },
            Seed {
                name: "Fedora-Workstation-Live-42.iso",
                source: "download.fedoraproject.org",
                total: 2.4 * GB,
                progress: 0.384,
                speed_mb: 9.4,
                status: Status::Paused,
                torrent: false,
                note: "虚拟机镜像",
            },
            Seed {
                name: "rust-1.86.0-x86_64-unknown-linux-gnu.tar.gz",
                source: "static.rust-lang.org",
                total: 246.0 * MB,
                progress: 1.0,
                speed_mb: 0.0,
                status: Status::Completed,
                torrent: false,
                note: "",
            },
            Seed {
                name: "debian-12.9.0-amd64-netinst.iso",
                source: "cdimage.debian.org",
                total: 629.0 * MB,
                progress: 0.126,
                speed_mb: 0.0,
                status: Status::Failed,
                torrent: false,
                note: "校验失败，待重下",
            },
            Seed {
                name: "openSUSE-Leap-15.6-DVD-x86_64.iso",
                source: "download.opensuse.org",
                total: 4.7 * GB,
                progress: 0.0,
                speed_mb: 7.8,
                status: Status::Queued,
                torrent: false,
                note: "",
            },
            Seed {
                name: "Noto_Sans_CJK_SC-v2.004.zip",
                source: "fonts.google.com",
                total: 118.0 * MB,
                progress: 1.0,
                speed_mb: 0.0,
                status: Status::Completed,
                torrent: false,
                note: "界面字体",
            },
            Seed {
                name: "archlinux-2026.10.01-x86_64.iso",
                source: "archlinux.org",
                total: 1.1 * GB,
                progress: 1.0,
                speed_mb: 0.0,
                status: Status::Seeding,
                torrent: true,
                note: "做种 1.8",
            },
            Seed {
                name: "JetBrains-Toolbox-2.5.3.AppImage",
                source: "download.jetbrains.com",
                total: 158.0 * MB,
                progress: 0.221,
                speed_mb: 4.1,
                status: Status::Downloading,
                torrent: false,
                note: "",
            },
            Seed {
                name: "VSCode-linux-x64-1.99.tar.gz",
                source: "update.code.visualstudio.com",
                total: 132.0 * MB,
                progress: 0.638,
                speed_mb: 3.2,
                status: Status::Paused,
                torrent: false,
                note: "",
            },
            Seed {
                name: "Aether-0.1.0-x86_64.AppImage",
                source: "github.com",
                total: 42.0 * MB,
                progress: 1.0,
                speed_mb: 0.0,
                status: Status::Completed,
                torrent: false,
                note: "本项目构建产物",
            },
        ];

        for seed in seeds {
            let id = self.next_id;
            self.next_id += 1;
            let order = shuffled_order(&mut self.rng);
            let nominal = seed.speed_mb * MB;
            let mut task = Task {
                id,
                name: seed.name.to_string(),
                source: seed.source.to_string(),
                path: "/home/aether/Downloads".to_string(),
                note: seed.note.to_string(),
                total: seed.total as u64,
                done: seed.total * seed.progress.max(0.0) as f64,
                status: seed.status,
                torrent: seed.torrent,
                // Completed tasks keep a plausible history even though their
                // live speed is zero.
                nominal_speed: nominal.max(6.0 * MB),
                speed: if seed.status == Status::Downloading {
                    nominal
                } else {
                    0.0
                },
                history: VecDeque::new(),
                order,
            };
            prefill_history(&mut task);
            self.tasks.push(task);
        }

        // A torrent that is still building up: order already partially done.
        self.tasks.sort_by(|a, b| b.id.cmp(&a.id));
    }
}

// ---------------------------------------------------------------------------
// Chart geometry
// ---------------------------------------------------------------------------

/// Builds the `Path` commands for the speed chart in a 0..1 viewbox.
/// Returns `(line, area)`.
pub fn chart_paths(history: &VecDeque<f64>) -> (String, String) {
    if history.is_empty() {
        return (String::new(), String::new());
    }
    let peak = history.iter().copied().fold(1.0_f64, f64::max).max(1.0);
    let n = history.len();
    let mut line = String::with_capacity(n * 18);
    let mut area = String::with_capacity(n * 18 + 24);
    area.push_str("M 0 1");

    for (i, value) in history.iter().enumerate() {
        let x = i as f64 / (n.saturating_sub(1)).max(1) as f64;
        let y = 0.94 - (value / peak).clamp(0.0, 1.0) * 0.88;
        if i == 0 {
            line.push_str(&format!("M {x:.4} {y:.4}"));
        } else {
            line.push_str(&format!(" L {x:.4} {y:.4}"));
        }
        area.push_str(&format!(" L {x:.4} {y:.4}"));
    }
    area.push_str(" L 1 1 Z");
    (line, area)
}

/// Peak of the history, used for the chart caption.
pub fn chart_peak(history: &VecDeque<f64>) -> f64 {
    history.iter().copied().fold(0.0, f64::max)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prefill_history(task: &mut Task) {
    let base = if task.speed > 0.0 {
        task.speed
    } else {
        task.nominal_speed
    };
    let mut rng = 0x1234_5678 ^ (task.id as u64) << 7;
    for i in 0..HISTORY_LEN {
        let ramp = if task.status == Status::Queued {
            0.0
        } else {
            let t = i as f64 / HISTORY_LEN as f64;
            (0.35 + 0.65 * t).min(1.0)
        };
        let wobble = 0.72 + rand_f32(&mut rng) as f64 * 0.5;
        task.push_sample(base * ramp * wobble);
    }
    task.push_sample(base);
}

fn log_line(at: u64, level: &'static str, message: String) -> LogEntry {
    LogEntry {
        time: format_clock(at),
        level,
        message,
    }
}

fn format_clock(seconds: u64) -> String {
    let s = seconds % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    format!("…/{}", parts[parts.len() - 1])
}

fn file_name_from_url(url: &str) -> String {
    let trimmed = url.split(['?', '#']).next().unwrap_or(url);
    let last = trimmed.rsplit('/').find(|s| !s.is_empty());
    match last {
        Some(name) if name.contains('.') => name.to_string(),
        Some(name) => format!("{name}"),
        None if !url.trim().is_empty() => "新的下载任务".to_string(),
        None => "未命名任务".to_string(),
    }
}

fn shuffled_order(rng: &mut u64) -> [u8; CHUNK_COUNT] {
    let mut order: [u8; CHUNK_COUNT] = core::array::from_fn(|i| i as u8);
    for i in (1..CHUNK_COUNT).rev() {
        let j = (rand_u32(rng) as usize) % (i + 1);
        order.swap(i, j);
    }
    order
}

fn rand_u32(state: &mut u64) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x >> 32) as u32
}

fn rand_f32(state: &mut u64) -> f32 {
    rand_u32(state) as f32 / u32::MAX as f32
}
