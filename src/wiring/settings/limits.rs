//! Scheduler window, concurrency cap, retries and the global speed cap.

use serde_json::json;

use crate::i18n;
use crate::state::State;
use crate::AppWindow;

pub(super) fn apply(
    st: &mut State,
    ui: Option<&AppWindow>,
    key: &str,
    value: &str,
) -> Option<String> {
    let mut hint = None;
    match key {
        "schedule-on" => {
            let enabled = value == "true";
            let (start, end) = (
                ui.map(|ui| ui.get_settings_schedule_start())
                    .unwrap_or_default()
                    .to_string(),
                ui.map(|ui| ui.get_settings_schedule_end())
                    .unwrap_or_default()
                    .to_string(),
            );
            if enabled && (start.trim().is_empty() || end.trim().is_empty()) {
                // Revert the switch: the daemon never heard about it.
                if let Some(ui) = ui {
                    ui.set_settings_schedule_on(false);
                }
                hint = Some(i18n::tr(
                    "请先填写开始与结束时间",
                    "Enter the start and end times first",
                ));
            } else {
                st.backend.set_setting(json!({"schedule_on": enabled}));
                hint = Some(if enabled {
                    i18n::tr(
                        "计划已开启：时段外自动暂停队列",
                        "Schedule on; the queue pauses outside the window",
                    )
                } else {
                    i18n::tr(
                        "计划已关闭：队列不再按时间暂停",
                        "Schedule off; the queue no longer pauses by time",
                    )
                });
            }
        }
        "schedule-start" | "schedule-end" => {
            let key = if key == "schedule-start" {
                "schedule_start"
            } else {
                "schedule_end"
            };
            st.backend.set_setting(json!({key: value}));
            hint = Some(i18n::tr("时间已保存（HH:MM）", "Times saved (HH:MM)"));
        }
        // Scheduler limits go through the same set_settings RPC; the daemon
        // validates them and applies them to the running queue. Tasks already
        // downloading keep the budget they started with.
        "max-tasks" => match value.trim().parse::<u64>() {
            Ok(number) => {
                st.backend.set_setting(json!({"max_concurrent": number}));
                hint = Some(i18n::tr(
                    "并发上限已保存：排队任务按新上限启动",
                    "Concurrency cap saved; queued tasks start under the new cap",
                ));
            }
            Err(_) => {
                hint = Some(i18n::tr(
                    format!("并发上限「{value}」无效"),
                    format!("Invalid concurrency cap: {value}"),
                ))
            }
        },
        "max-connections" => match value.trim().parse::<u64>() {
            Ok(number) => {
                st.backend.set_setting(json!({"connections": number}));
                hint = Some(i18n::tr(
                    "每任务连接数已保存：新任务生效（HTTP 上限 16）",
                    "Connections per task saved; applies to new tasks (HTTP capped at 16)",
                ));
            }
            Err(_) => {
                hint = Some(i18n::tr(
                    format!("每任务连接数「{value}」无效"),
                    format!("Invalid connection count: {value}"),
                ))
            }
        },
        "retry-count" => {
            let retries = match value {
                "不重试" => Some(0_u64),
                other => other.trim().parse::<u64>().ok(),
            };
            match retries {
                Some(number) => {
                    st.backend.set_setting(json!({"retry_max": number}));
                    hint = Some(if number == 0 {
                        i18n::tr("已关闭自动重试", "Automatic retries are off")
                    } else {
                        i18n::tr(
                            "重试上限已保存：下次失败时生效",
                            "Retry cap saved; applies from the next failure",
                        )
                    });
                }
                None => {
                    hint = Some(i18n::tr(
                        format!("重试次数「{value}」无效"),
                        format!("Invalid retry count: {value}"),
                    ))
                }
            }
        }
        // One knob, two controls: the switch sends "no limit" (0), the slider
        // sends the rate in bytes per second.
        "global-limit" => {
            st.global_limit = value == "true";
            let bps = if st.global_limit {
                (st.download_limit_kb * 1024.0) as u64
            } else {
                0
            };
            st.backend.set_setting(json!({"max_speed_bps": bps}));
            hint = Some(if st.global_limit {
                i18n::tr(
                    format!("全局限速已保存：{} KB/s，新任务生效", st.download_limit_kb),
                    format!(
                        "Global cap saved: {} KB/s, applies to new tasks",
                        st.download_limit_kb
                    ),
                )
            } else {
                i18n::tr("已关闭全局限速", "The global speed cap is off")
            });
        }
        "download-limit" => match value.trim().parse::<f64>() {
            Ok(kb) if kb >= 0.0 => {
                st.download_limit_kb = kb;
                if st.global_limit {
                    st.backend
                        .set_setting(json!({"max_speed_bps": (kb * 1024.0) as u64}));
                    hint = Some(i18n::tr(
                        format!("下载限速已保存：{kb} KB/s，新任务生效"),
                        format!("Download cap saved: {kb} KB/s, applies to new tasks"),
                    ));
                } else {
                    hint = Some(i18n::tr(
                        "已记录限速，启用全局限速后生效",
                        "Rate recorded; applies once the global speed cap is on",
                    ));
                }
            }
            _ => {
                hint = Some(i18n::tr(
                    format!("限速值「{value}」无效"),
                    format!("Invalid speed limit: {value}"),
                ))
            }
        },
        _ => {}
    }
    hint
}
