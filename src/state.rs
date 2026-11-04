//! Window state shared by every handler and refresh pass.

use std::rc::Rc;
use std::time::Instant;

use aether_daemon::PluginInfo;
use slint::VecModel;

use crate::backend::Backend;
use crate::clipboard;
use crate::model::{self, Status, CHUNK_COUNT};
use crate::plugins::browser_extension_installed;
use crate::prefs::UiPrefs;
use crate::{DetailSection, RecordRow, TaskRow};

pub(crate) struct State {
    pub(crate) backend: Backend,
    pub(crate) filter: i32,
    pub(crate) query: String,
    pub(crate) detail_id: i32,
    pub(crate) rows: Rc<VecModel<TaskRow>>,
    pub(crate) records: Rc<VecModel<RecordRow>>,
    pub(crate) chunks: Rc<VecModel<f32>>,
    /// Ids of the rows currently materialised in `rows`, in order.
    pub(crate) row_ids: Vec<i32>,
    /// Ids of the records currently materialised in `records`, in order.
    pub(crate) record_ids: Vec<i32>,
    /// When the current toast should disappear.
    pub(crate) toast_until: Option<Instant>,
    /// Task id the chunk model was seeded for.
    pub(crate) detail_data_for: i32,
    /// Generic detail sections for the drawer. The model instance stays
    /// stable while the drawer is open so the peers list keeps its scroll
    /// position across payload refreshes.
    pub(crate) sections: Rc<VecModel<DetailSection>>,
    /// (task id, detail payload JSON) the sections model was built from.
    pub(crate) sections_sig: Option<(i32, String)>,
    /// Set once the window property points at `sections`.
    pub(crate) sections_attached: bool,
    /// Last proxy URL typed on the settings page, so switching the mode can
    /// push a complete proxy setting.
    pub(crate) proxy_url: String,
    pub(crate) proxy_mode: String,
    /// Global speed cap as the download page shows it: the switch state and
    /// the KB/s the slider holds (kept while the cap is off, so switching it
    /// back on restores the previous rate).
    pub(crate) global_limit: bool,
    pub(crate) download_limit_kb: f64,
    /// Settings JSON last pushed into the window, so the mirror is only
    /// written when the daemon actually reported a change.
    pub(crate) pushed_settings: Option<serde_json::Value>,
    /// UI-local preferences (accent, motion, flow, start page, after-done).
    pub(crate) prefs: UiPrefs,
    /// Last page index seen in the window, for "上次页面" persistence.
    pub(crate) seen_page: i32,
    pub(crate) last_notice: Option<String>,
    /// Installed plugins mirrored from the snapshot (row id → plugin id).
    pub(crate) plugins: Vec<PluginInfo>,
    /// Change signature so the catalog model is only rebuilt on real change.
    pub(crate) plugins_sig: String,
    /// Session start, for the dashboard uptime row.
    pub(crate) started: Instant,
    /// Whether a browser extension native-host manifest is present.
    pub(crate) extension_installed: bool,
    /// Clipboard link watcher: `None` when the session has no clipboard
    /// backend (the settings row says so instead of faking availability).
    pub(crate) clipboard: Option<clipboard::ClipboardWatch>,
    /// Flips the poller from the settings page.
    pub(crate) clipboard_control: Option<clipboard::ClipboardControl>,
    /// URL staged by the latest detection; the toast action opens it.
    pub(crate) clipboard_url: Option<String>,
    /// A user-triggered update check that has not reported its result yet.
    pub(crate) update_watch: Option<UpdateWatch>,
    /// Snapshot last painted and the uptime minute at that moment. The tick
    /// skips the whole row pass while both are unchanged: every setter
    /// dirties the window, so an idle app would otherwise repaint at 5 Hz.
    pub(crate) last_painted: Option<(crate::backend::Snapshot, u64)>,
    /// When the ambient drift was last pushed. It is a slow sine whose layers
    /// move well under a pixel per second, so 1 Hz looks the same as 5 Hz.
    pub(crate) drift_at: Option<Instant>,
}

/// Tracks one on-demand update check so the UI can toast the final state
/// instead of leaving "checking…" hanging forever.
pub(crate) struct UpdateWatch {
    pub(crate) since: Instant,
    pub(crate) saw_checking: bool,
}

impl State {
    pub(crate) fn new() -> Self {
        State {
            backend: Backend::spawn(),
            filter: 0,
            query: String::new(),
            detail_id: -1,
            rows: Rc::new(VecModel::default()),
            records: Rc::new(VecModel::default()),
            chunks: Rc::new(VecModel::from(vec![0.0_f32; CHUNK_COUNT])),
            row_ids: Vec::new(),
            record_ids: Vec::new(),
            toast_until: None,
            detail_data_for: -1,
            sections: Rc::new(VecModel::default()),
            sections_sig: None,
            sections_attached: false,
            proxy_url: String::new(),
            proxy_mode: "跟随系统".into(),
            global_limit: true,
            download_limit_kb: 2048.0,
            pushed_settings: None,
            prefs: UiPrefs::load(),
            seen_page: 0,
            last_notice: None,
            clipboard: None,
            clipboard_control: None,
            clipboard_url: None,
            update_watch: None,
            last_painted: None,
            drift_at: None,
            plugins: Vec::new(),
            plugins_sig: String::new(),
            started: Instant::now(),
            extension_installed: browser_extension_installed(),
        }
    }

    pub(crate) fn matches(&self, task: &model::Task) -> bool {
        let status_ok = match self.filter {
            0 => true,
            1 => task.status == Status::Downloading,
            2 => task.status == Status::Completed,
            3 => task.status == Status::Paused,
            4 => task.status == Status::Failed,
            5 => task.torrent,
            _ => true,
        };
        if !status_ok {
            return false;
        }
        if self.query.is_empty() {
            return true;
        }
        let needle = self.query.to_lowercase();
        task.name.to_lowercase().contains(&needle)
            || task.source.to_lowercase().contains(&needle)
            || task.note.to_lowercase().contains(&needle)
    }

    pub(crate) fn visible_ids(&self, tasks: &[model::Task]) -> Vec<i32> {
        tasks
            .iter()
            .filter(|task| self.matches(task))
            .map(|task| task.id)
            .collect()
    }
}
