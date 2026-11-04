//! Per-plugin settings dialog: schema to form, form to validated JSON.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::i18n;
use crate::plugin_settings::{plugin_settings_form, plugin_settings_json, FormError};
use crate::state::State;
use crate::toast::show_toast;
use crate::{AppWindow, PluginSettings};

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // Rust owns the form: `configure` pushes fresh fields + values (schema
    // order, seeded from the stored config) into the exported global; `save`
    // rebuilds the JSON object, validates it and persists through the daemon.
    // A failed parse toasts and returns false so the dialog stays open.
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.global::<PluginSettings>().on_configure(move |row| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let st = state.borrow();
            let Some(info) = st.plugins.get(row as usize) else {
                return;
            };
            let (fields, values) = plugin_settings_form(info);
            if fields.is_empty() {
                return;
            }
            let form = ui.global::<PluginSettings>();
            form.set_row(row);
            form.set_title(info.name.clone().into());
            form.set_fields(ModelRc::new(VecModel::from(fields)));
            form.set_values(ModelRc::new(VecModel::from(
                values
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )));
            form.set_open(true);
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.global::<PluginSettings>().on_save(move || -> bool {
            let Some(ui) = ui_handle.upgrade() else {
                return false;
            };
            let form = ui.global::<PluginSettings>();
            let row = form.get_row();
            let fields = form.get_fields();
            let values = form.get_values();
            // One text slot per declared field; anything else means the
            // dialog never finished opening.
            if fields.row_count() != values.row_count() {
                return false;
            }
            let outcome = (|| -> Result<(String, serde_json::Value), String> {
                let st = state.borrow();
                let Some(info) = st.plugins.get(row as usize) else {
                    return Err(i18n::tr(
                        "该插件已不在列表中",
                        "That plugin is no longer listed",
                    ));
                };
                let settings = plugin_settings_json(&fields, &values, &info.settings)
                    .map_err(FormError::message)?;
                Ok((info.id.clone(), serde_json::Value::Object(settings)))
            })();
            match outcome {
                Ok((plugin_id, settings)) => {
                    state
                        .borrow()
                        .backend
                        .set_plugin_settings(&plugin_id, settings);
                    true
                }
                Err(message) => {
                    show_toast(&ui, &state, &message);
                    false
                }
            }
        });
    }
}
