//! Task list actions: row buttons, filter/search and new-download dialog.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::split_urls;
use crate::detail::open_detail;
use crate::i18n;
use crate::model::Status;
use crate::rows::refresh_rows;
use crate::state::State;
use crate::toast::show_toast;
use crate::AppWindow;

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_task_action(move |id, action| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let message = {
                let mut st = state.borrow_mut();
                let snapshot = st.backend.snapshot();
                let was_failed = snapshot
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.status == Status::Failed)
                    .unwrap_or(false);
                let existed = snapshot.tasks.iter().any(|task| task.id == id);
                st.backend.apply_action(id, action);
                if action == 2 && st.detail_id == id {
                    st.detail_id = -1;
                }
                match action {
                    1 if was_failed => {
                        Some(i18n::tr("失败任务已重新开始下载", "Failed task restarted"))
                    }
                    2 if existed => {
                        Some(i18n::tr("任务已从列表中移除", "Task removed from the list"))
                    }
                    3 if existed => Some(i18n::tr(
                        "已在文件管理器中定位到文件",
                        "Revealed in the file manager",
                    )),
                    _ => None,
                }
            };
            let closed = state.borrow().detail_id < 0;
            if closed {
                ui.set_drawer_open(false);
            }
            refresh_rows(&ui, &state);
            if let Some(message) = message {
                show_toast(&ui, &state, &message);
            }
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_open_detail(move |id| {
            if let Some(ui) = ui_handle.upgrade() {
                open_detail(&ui, &state, id);
            }
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_drawer_dismissed(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            state.borrow_mut().detail_id = -1;
            refresh_rows(&ui, &state);
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_filter_changed(move |filter, query| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = filter;
                st.query = query.to_string();
            }
            refresh_rows(&ui, &state);
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_reset_filters(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query.clear();
            }
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_new_download(move |url, path, _segments, limit_kb, _note| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let urls = split_urls(&url);
            let count = urls.len();
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query = String::new();
                for url in &urls {
                    st.backend.add_task(url, &path, limit_kb);
                }
            }
            ui.set_page(1);
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
            // One URL is the plain case; a pasted batch reports how many
            // tasks were queued (per-URL failures arrive as daemon notices
            // afterwards).
            let message = if count > 1 {
                i18n::tr(
                    format!("已添加 {count} 个下载任务"),
                    format!("{count} downloads queued"),
                )
            } else {
                i18n::tr("已添加下载任务", "Download queued")
            };
            show_toast(&ui, &state, &message);
        });
    }
}
