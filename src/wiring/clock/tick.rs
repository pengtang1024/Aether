//! One clock tick: drift, notices, clipboard offers, snapshot painting,
//! update results and after-done actions.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::detail::refresh_detail;
use crate::i18n;
use crate::platform::{open_path, send_notification};
use crate::state::State;
use crate::toast::{show_action_toast, show_toast};
use crate::update_status::update_status_text;
use crate::AppWindow;

/// Sub-pixel drift cadence; see `tick_body`.
const DRIFT_INTERVAL: Duration = Duration::from_secs(1);

pub(super) fn tick_body(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let drawer_open = ui.get_drawer_open();
    // The ambient drift is a 28 s sine: sampling it at 1 Hz moves the layers
    // by at most 0.22 px per step, visually identical to 5 Hz but only
    // repaints once a second. Everything else keeps its tick rate.
    let drift_due = {
        let mut st = state.borrow_mut();
        let due = st.drift_at.is_none_or(|at| at.elapsed() >= DRIFT_INTERVAL);
        if due {
            st.drift_at = Some(Instant::now());
        }
        due
    };
    if drift_due {
        let drift = state.borrow().backend.drift();
        ui.set_drift(drift);
    }
    let (expire_toast, notice) = {
        let mut st = state.borrow_mut();
        let notice = st.backend.take_notice().filter(|notice| {
            if st.last_notice.as_deref() == Some(notice.as_str()) {
                false
            } else {
                st.last_notice = Some(notice.clone());
                true
            }
        });
        let expire_toast = st.toast_until.is_some_and(|until| Instant::now() >= until);
        if expire_toast {
            st.toast_until = None;
        }
        (expire_toast, notice)
    };
    if expire_toast {
        ui.set_toast_open(false);
    }
    if let Some(text) = notice {
        show_toast(ui, state, &text);
    }
    // Clipboard detections: stage the URL and offer one-click start.
    {
        let detected = state
            .borrow()
            .clipboard
            .as_ref()
            .map(|watch| watch.drain())
            .unwrap_or_default();
        for link in detected {
            // Stage first, then drop the borrow: show_action_toast
            // borrows the state itself.
            state.borrow_mut().clipboard_url = Some(link.url);
            show_action_toast(
                ui,
                state,
                &i18n::tr("检测到下载链接", "Download link detected"),
                &i18n::tr("新建任务", "New task"),
            );
        }
    }
    // One snapshot per tick: rows and the detail drawer share it instead
    // of each cloning every task, plugin and detail payload. Applying a
    // snapshot dirties the window on every setter, so skip the pass while
    // the data (and the uptime minute) has not moved.
    let snapshot = state.borrow().backend.snapshot();
    let uptime_minute = state.borrow().started.elapsed().as_secs() / 60;
    let repaint = {
        let mut st = state.borrow_mut();
        let repaint = super::needs_repaint(st.last_painted.as_ref(), &snapshot, uptime_minute);
        if repaint {
            st.last_painted = Some((snapshot.clone(), uptime_minute));
        }
        repaint
    };
    if repaint {
        crate::rows::apply_snapshot(ui, state, &snapshot);
    }
    // A user-triggered check is asynchronous: wait until the daemon
    // leaves `checking`, then toast the real result. Without this the
    // "checking…" toast vanished and the panel silently flipped back.
    {
        let outcome = {
            let mut st = state.borrow_mut();
            let update = st.backend.update_status();
            match (st.update_watch.as_mut(), update) {
                (None, _) => None,
                (Some(watch), None) => {
                    if watch.since.elapsed() > Duration::from_secs(150) {
                        st.update_watch = None;
                        Some(i18n::tr(
                            "检查更新超时：daemon 没有返回状态",
                            "Update check timed out: the daemon reported no state",
                        ))
                    } else {
                        None
                    }
                }
                (Some(watch), Some(update)) => {
                    let status = update["status"].as_str().unwrap_or("");
                    if status == "checking" {
                        watch.saw_checking = true;
                        None
                    } else if watch.saw_checking || watch.since.elapsed() > Duration::from_secs(2) {
                        st.update_watch = None;
                        Some(update_status_text(Some(&update)))
                    } else {
                        None
                    }
                }
            }
        };
        if let Some(message) = outcome {
            show_toast(ui, state, &message);
        }
    }
    if drawer_open && repaint {
        refresh_detail(ui, state, &snapshot);
    }
    // Persist the page for "上次页面"; run the after-done action for
    // tasks that completed since the last tick.
    {
        let mut st = state.borrow_mut();
        let page = ui.get_page();
        if page != st.seen_page {
            st.seen_page = page;
            st.prefs.last_page = page;
            st.prefs.save();
        }
        let mut done = Vec::new();
        st.backend.take_finished(&mut done);
        let after = st.prefs.after_done;
        let notify_done = st.prefs.notify_done;
        let notify_fail = st.prefs.notify_fail;
        drop(st);
        for item in done {
            if item.done {
                if notify_done {
                    send_notification(
                        &i18n::tr("下载完成", "Download completed"),
                        &i18n::tr(
                            format!("「{}」已下载完成", item.name),
                            format!("“{}” has finished downloading", item.name),
                        ),
                    );
                }
                match after {
                    1 => {
                        if let Some(dir) = &item.dir {
                            let _ = open_path(std::path::Path::new(dir));
                        }
                    }
                    2 => {
                        if let (Some(dir), Some(file)) = (&item.dir, &item.file) {
                            let _ = open_path(&std::path::Path::new(dir).join(file));
                        }
                    }
                    _ => {}
                }
            } else if notify_fail {
                let detail = item.error.as_deref().unwrap_or_default();
                let body = if detail.is_empty() {
                    i18n::tr(
                        format!("「{}」下载失败", item.name),
                        format!("“{}” failed to download", item.name),
                    )
                } else {
                    i18n::tr(
                        format!("「{}」下载失败：{detail}", item.name),
                        format!("“{}” failed to download: {detail}", item.name),
                    )
                };
                send_notification(&i18n::tr("下载失败", "Download failed"), &body);
            }
        }
    }
}
