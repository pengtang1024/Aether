//! Plugin catalogue model, rebuilt only when the registry signature changes.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::{ModelRc, VecModel};

use crate::backend::Snapshot;
use crate::plugin_settings::plugin_settings_form;
use crate::plugins::{plugin_blurb, plugin_caps, plugin_category, plugin_icon, plugin_tone};
use crate::state::State;
use crate::{AppWindow, PluginCatalog, PluginRow};

pub(super) fn apply(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    let mut st = state.borrow_mut();

    // `configured: false` means the daemon has no update channel; never turn
    // that into "up to date".
    let plugin_updates = st.backend.plugin_updates();
    let update_ids: Vec<&str> = plugin_updates
        .as_ref()
        .filter(|value| value["configured"].as_bool().unwrap_or(false))
        .and_then(|value| value["updates"].as_array())
        .map(|updates| {
            updates
                .iter()
                .filter_map(|update| update["plugin_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let sig = snapshot
        .plugins
        .iter()
        .map(|plugin| {
            format!(
                "{}:{}:{}:{}:{}",
                plugin.id,
                plugin.version,
                plugin.enabled,
                update_ids.contains(&plugin.id.as_str()),
                // Stored settings ride along: they change without a version
                // bump, but the dialog reads them from `st.plugins`.
                plugin.settings
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    if sig != st.plugins_sig {
        st.plugins_sig = sig;
        st.plugins = snapshot.plugins.clone();
        // Clipboard detection follows the protocols the daemon actually has.
        if let Some(control) = st.clipboard_control.as_ref() {
            let protocols: Vec<String> = snapshot
                .plugins
                .iter()
                .flat_map(|plugin| plugin.protocols.iter().cloned())
                .collect();
            control.set_protocols(&protocols);
        }
        let total = snapshot.plugins.len() as i32;
        let enabled = snapshot.plugins.iter().filter(|p| p.enabled).count() as i32;
        let update_count = snapshot
            .plugins
            .iter()
            .filter(|plugin| update_ids.contains(&plugin.id.as_str()))
            .count() as i32;
        let rows: Vec<PluginRow> = snapshot
            .plugins
            .iter()
            .enumerate()
            .map(|(index, info)| PluginRow {
                id: index as i32,
                name: info.name.clone().into(),
                description: plugin_blurb(info).into(),
                caps: plugin_caps(info).into(),
                version: info.version.clone().into(),
                category: plugin_category(&info.id).into(),
                kind: 0,
                enabled: info.enabled,
                blocked: info.blocked,
                service: info.service,
                running: info.running,
                error: info.last_error.clone().unwrap_or_default().into(),
                update: update_ids.contains(&info.id.as_str()),
                featured: false,
                can_rollback: info.has_backup,
                icon: plugin_icon(&info.id),
                tone: plugin_tone(&info.id),
                // Configure affordance only when the daemon declared a
                // schema with at least one renderable field.
                can_configure: !plugin_settings_form(info).0.is_empty(),
            })
            .collect();
        ui.global::<PluginCatalog>()
            .set_all(ModelRc::new(VecModel::from(rows)));
        ui.global::<PluginCatalog>().set_total(total);
        ui.global::<PluginCatalog>().set_active(enabled);
        ui.global::<PluginCatalog>().set_builtin(total);
        ui.global::<PluginCatalog>().set_community(0);
        ui.global::<PluginCatalog>().set_updates(update_count);
    }
}
