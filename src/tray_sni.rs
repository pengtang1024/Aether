//! StatusNotifierItem system tray (Linux, session bus).
//!
//! Needs a StatusNotifier host to display: KDE ships one, GNOME needs the
//! AppIndicator extension; on a session without a bus or host — startup
//! degrades to "no tray" with one log line and the app behaves exactly as
//! before. Downloads keep running in the daemon either way: the tray only
//! summons or quits the window.
//!
//! Protocol surface is the pragmatic subset: StatusNotifierItem with
//! Activate (show) plus a com.canonical.dbusmenu menu (显示主窗口/退出)
//! exported at `/Menu` and `/MenuBar` because hosts disagree on the path.
//!
//! A host can come and go (the AppIndicator extension is reloaded, the
//! panel restarts). The icon is re-announced whenever the watcher's unique
//! bus name changes, so a live window is never left without its icon.
//! A second launch asks the window back through `org.aether.UI` instead of
//! only showing an "already running" toast.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use zbus::interface;
use zbus::zvariant::{Array, Dict, DynamicType, ObjectPath, Signature, Structure, Type, Value};

const WATCHER_SERVICE: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Well-known name the running instance answers a second launch on.
const UI_SERVICE: &str = "org.aether.UI";
const UI_PATH: &str = "/org/aether/UI";
/// Startup waits this long for the tray handshake. A wedged session bus must
/// not freeze the window beyond it.
const HANDSHAKE: Duration = Duration::from_secs(5);
/// How often the tray checks that the host it registered with is still the
/// same process. A restart (extension reload, panel restart) otherwise
/// leaves a hidden window behind an icon that no longer exists.
const HOST_CHECK: Duration = Duration::from_secs(5);

/// A signature literal that is correct by construction; `expect` documents
/// that a failure here would be a source-code bug, not runtime input.
fn lit_sig(sig: &str) -> Signature {
    Signature::try_from(sig).expect("static signature literal")
}

struct TrayShared {
    window: slint::Weak<crate::AppWindow>,
}

impl TrayShared {
    fn show(&self) {
        let window = self.window.clone();
        // Tray callbacks arrive on the zbus serving thread.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = window.upgrade() {
                crate::tray::present(&ui);
            }
        });
    }

    fn quit(&self) {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    }
}

/// Start the tray on a dedicated thread. `None` = no session bus or no
/// StatusNotifier host; the reason is logged once and the window stays the
/// only entry point.
pub fn start(window: slint::Weak<crate::AppWindow>) -> Option<()> {
    let shared = Arc::new(TrayShared { window });
    // A timed-out handshake must not leave an icon behind later: the app has
    // already decided "no tray" (close = quit), so the thread is told to drop
    // whatever it managed to register.
    let cancel = Arc::new(AtomicBool::new(false));
    let tray_cancel = cancel.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || run(shared, tray_cancel, ready_tx));
    match ready_rx.recv_timeout(HANDSHAKE) {
        Ok(Ok(())) => Some(()),
        Ok(Err(error)) => {
            eprintln!("aether: no system tray ({error})");
            None
        }
        Err(_) => {
            cancel.store(true, Ordering::SeqCst);
            eprintln!(
                "aether: no system tray (no answer within {}s)",
                HANDSHAKE.as_secs()
            );
            None
        }
    }
}

fn run(shared: Arc<TrayShared>, cancel: Arc<AtomicBool>, ready: mpsc::Sender<Result<(), String>>) {
    let conn = match zbus::blocking::Connection::session() {
        Ok(conn) => conn,
        Err(error) => {
            let _ = ready.send(Err(format!("session bus: {error}")));
            return;
        }
    };
    let service = format!("org.kde.StatusNotifierItem-{}", std::process::id());

    let export = conn
        .object_server()
        .at(
            "/StatusNotifierItem",
            Item {
                shared: shared.clone(),
            },
        )
        .and_then(|_| {
            conn.object_server().at(
                MENU_PATH,
                Menu {
                    shared: shared.clone(),
                },
            )
        })
        .and_then(|_| {
            conn.object_server().at(
                "/MenuBar",
                Menu {
                    shared: shared.clone(),
                },
            )
        })
        .and_then(|_| {
            conn.object_server().at(
                UI_PATH,
                Ui {
                    shared: shared.clone(),
                },
            )
        });
    if let Err(error) = export {
        let _ = ready.send(Err(format!("export: {error}")));
        return;
    }
    if let Err(error) = conn.request_name(service.as_str()) {
        let _ = ready.send(Err(format!("bus name: {error}")));
        return;
    }
    // A second launch reaches the running window here. Best effort: the
    // single-instance lock already keeps one UI per user, so a failure only
    // costs the summon (the second launch then shows the toast).
    let _ = conn.request_name(UI_SERVICE);
    // No watcher means no visible icon: report the honest "no tray" so the
    // window keeps its default close behaviour (quitting) instead of hiding
    // into an icon that does not exist.
    if let Err(error) = register_with_watcher(&conn, &service) {
        let _ = ready.send(Err(format!("no StatusNotifier host: {error}")));
        return;
    }
    if cancel.load(Ordering::SeqCst) {
        // The startup handshake timed out while we were registering; dropping
        // the connection releases the bus name and the icon with it.
        return;
    }
    // Record the startup language so the first host-check tick does not
    // broadcast a change that never happened.
    let _ = next_revision();
    let _ = ready.send(Ok(()));
    if cancel.load(Ordering::SeqCst) {
        // The startup handshake timed out just as registration finished;
        // dropping the connection releases the icon with it.
        return;
    }
    keep_registered(&conn, &service, &cancel);
}

/// Re-announce the item whenever the StatusNotifier watcher is replaced.
/// The watcher's unique bus name is stable for the lifetime of one host
/// process, so a change means the host restarted and forgot every item.
fn keep_registered(conn: &zbus::blocking::Connection, service: &str, cancel: &AtomicBool) {
    // The connection is owned by this thread; the zbus serving thread only
    // borrows it for incoming calls.
    let mut owner = watcher_owner(conn);
    loop {
        thread::sleep(HOST_CHECK);
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        announce_language_change(conn);
        let current = watcher_owner(conn);
        if current == owner && owner.is_some() {
            continue;
        }
        match current {
            Some(_) => {
                owner = if register_with_watcher(conn, service).is_ok() {
                    current
                } else {
                    None
                };
            }
            // No host right now: retry on a later tick (the icon disappears
            // with its host and comes back with the next registration).
            None => owner = None,
        }
    }
}

fn watcher_owner(conn: &zbus::blocking::Connection) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(WATCHER_SERVICE,),
        )
        .ok()?;
    let body = reply.body();
    body.deserialize::<String>().ok()
}

fn register_with_watcher(conn: &zbus::blocking::Connection, service: &str) -> Result<(), String> {
    let proxy = zbus::blocking::Proxy::new(conn, WATCHER_SERVICE, WATCHER_PATH, WATCHER_SERVICE)
        .map_err(|error| error.to_string())?;
    proxy
        .call_method("RegisterStatusNotifierItem", &(service,))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Ask the running UI to surface its window. `false` = no session bus or no
/// running UI endpoint (the caller then shows the "already running" toast).
pub fn summon() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    conn.call_method(
        Some(UI_SERVICE),
        UI_PATH,
        Some("org.aether.UI"),
        "Activate",
        &(),
    )
    .is_ok()
}

// ---------------------------------------------------------------------------
// org.aether.UI: the endpoint a second launch calls
// ---------------------------------------------------------------------------

struct Ui {
    shared: Arc<TrayShared>,
}

#[interface(name = "org.aether.UI")]
impl Ui {
    /// Surface the main window (the same path as the tray's Activate).
    fn activate(&self) {
        self.shared.show();
    }
}

// ---------------------------------------------------------------------------
// org.kde.StatusNotifierItem
// ---------------------------------------------------------------------------

/// The dbusmenu object path, exported below and announced through the
/// StatusNotifierItem `Menu` property (Ubuntu's AppIndicator extension treats
/// `Id` + `Menu` as required and drops the icon when either lookup fails).
const MENU_PATH: &str = "/Menu";

struct Item {
    shared: Arc<TrayShared>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn id(&self) -> &str {
        "aether"
    }

    #[zbus(property)]
    fn category(&self) -> &str {
        "Application"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    /// Askable at any time, so this follows the UI language instead of the
    /// language that happened to be active when the tray started.
    #[zbus(property)]
    fn title(&self) -> String {
        crate::i18n::tr("Aether 下载管理器", "Aether Download Manager")
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "aether"
    }

    /// Path of the dbusmenu object (exported below). Ubuntu's AppIndicator
    /// extension treats `Id` + `Menu` as required properties and silently
    /// drops the icon when either lookup fails — `ItemIsMenu` alone is not
    /// enough.
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        ObjectPath::from_static_str(MENU_PATH).expect("static object path")
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        true
    }

    /// Left click / the host's primary activation: summon the window.
    fn activate(&self, _x: i32, _y: i32) {
        self.shared.show();
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.shared.show();
    }

    fn scroll(&self, _n: i32, _orientation: &str) {}
}

// ---------------------------------------------------------------------------
// com.canonical.dbusmenu: 显示主窗口 / 退出
// ---------------------------------------------------------------------------

struct Menu {
    shared: Arc<TrayShared>,
}

const MENU_SHOW: i32 = 1;
const MENU_QUIT: i32 = 2;

/// Bumped only when the layout contents actually change (currently: a
/// language switch). Hosts cache by revision and re-read on open, so this
/// keeps a language change visible without an `LayoutUpdated` broadcast.
static MENU_REVISION: AtomicU32 = AtomicU32::new(1);
static MENU_LANGUAGE: AtomicU8 = AtomicU8::new(u8::MAX);

fn next_revision() -> u32 {
    let language = crate::i18n::index_of(crate::i18n::current_code()) as u8;
    if MENU_LANGUAGE.swap(language, Ordering::Relaxed) != language {
        MENU_REVISION.fetch_add(1, Ordering::Relaxed);
    }
    MENU_REVISION.load(Ordering::Relaxed)
}

/// Tell hosts that the labels changed (a language switch). `ItemsPropertiesUpdated`
/// is the dbusmenu signal for exactly this; `LayoutUpdated` alone would not
/// re-read the labels of items the host already knows.
fn announce_language_change(conn: &zbus::blocking::Connection) {
    let before = MENU_REVISION.load(Ordering::Relaxed);
    if next_revision() == before {
        return;
    }
    let updated: Vec<GroupEntry> = [0, MENU_SHOW, MENU_QUIT]
        .into_iter()
        .map(|id| GroupEntry {
            id,
            props: node_props(id, id == 0, &[]),
        })
        .collect();
    let removed: Vec<(i32, Vec<String>)> = Vec::new();
    let _ = conn.emit_signal(
        None::<&str>,
        MENU_PATH,
        "com.canonical.dbusmenu",
        "ItemsPropertiesUpdated",
        &(updated, removed),
    );
}

fn menu_label(id: i32) -> String {
    match id {
        0 => "Aether".to_string(),
        MENU_SHOW => crate::i18n::tr("显示主窗口", "Show main window"),
        MENU_QUIT => crate::i18n::tr("退出", "Quit"),
        _ => String::new(),
    }
}

/// One node's properties. `only` is the host's requested property list (empty
/// = all), so a `GetGroupProperties` call does not have to build keys it will
/// drop again.
fn node_props(id: i32, root: bool, only: &[String]) -> Dict<'static, 'static> {
    let mut props = Dict::new(&lit_sig("s"), &lit_sig("v"));
    let wanted = |name: &str| only.is_empty() || only.iter().any(|requested| requested == name);
    if wanted("label") {
        props
            .append(
                Value::Str("label".into()),
                Value::Value(Box::new(Value::Str(menu_label(id).into()))),
            )
            .expect("a{sv} key/value");
    }
    if wanted("enabled") {
        props
            .append(
                Value::Str("enabled".into()),
                Value::Value(Box::new(Value::Bool(true))),
            )
            .expect("a{sv} key/value");
    }
    if root && wanted("children-display") {
        props
            .append(
                Value::Str("children-display".into()),
                Value::Value(Box::new(Value::Str("submenu".into()))),
            )
            .expect("a{sv} key/value");
    }
    props
}

/// A field paired with its D-Bus signature. `Structure::from` derives the
/// struct signature from each element's `DynamicType`, and `Value`'s signature
/// is `v` — building a node from bare `Value`s therefore declares `(vvv)` and
/// writes every field as a variant. The host reads that as "children is not
/// an array" and never builds the menu (a click then does nothing at all).
struct Signed<'a> {
    signature: Signature,
    value: Value<'a>,
}

impl DynamicType for Signed<'_> {
    fn signature(&self) -> Signature {
        self.signature.clone()
    }
}

impl<'a> From<Signed<'a>> for Value<'a> {
    fn from(signed: Signed<'a>) -> Self {
        signed.value
    }
}

/// One menu item as a leaf node: D-Bus `(ia{sv}av)` with empty children.
fn leaf(id: i32, only: &[String]) -> Value<'static> {
    let node = Structure::from((
        Signed {
            signature: lit_sig("i"),
            value: Value::I32(id),
        },
        Signed {
            signature: lit_sig("a{sv}"),
            value: Value::Dict(node_props(id, false, only)),
        },
        Signed {
            signature: lit_sig("av"),
            value: Value::Array(Array::new(&lit_sig("v"))),
        },
    ));
    Value::Structure(node)
}

/// The menu root: `(0, {children-display: "submenu"}, av[leaf, leaf])`.
fn root_node(only: &[String]) -> LayoutNode {
    let mut array = Array::new(&lit_sig("v"));
    for id in [MENU_SHOW, MENU_QUIT] {
        array
            .append(Value::Value(Box::new(leaf(id, only))))
            .expect("av element");
    }
    LayoutNode {
        id: 0,
        props: node_props(0, true, only),
        children: array,
    }
}

/// One dbusmenu node as GetLayout returns it: D-Bus `(ia{sv}av)`. The
/// explicit signature pins the static type (zbus method returns need
/// `Type`); `props` serializes as a{sv} and `children` as av — an empty
/// `Value::Array` of variant elements keeps the signature even with no
/// grandchildren.
#[derive(Serialize, Type)]
#[zvariant(signature = "(ia{sv}av)")]
struct LayoutNode {
    id: i32,
    props: Dict<'static, 'static>,
    // NOT `Value`: serde-serializing a Value wraps it in a variant (`v`),
    // which silently breaks the declared `(ia{sv}av)`. Array/Dict serialize
    // with their declared signatures instead (the trap that made gdbus
    // report "got v" for GetLayout).
    children: Array<'static>,
}

/// GetLayout's out-args: `u32 revision, (ia{sv}av) layout` — **one** root
/// node, not an array. Ubuntu's AppIndicator (and the spec's XML) expect the
/// singular form; returning `a(ia{sv}av)` makes the host reject the reply with
/// a type error, never build the menu, and a click on the icon then does
/// nothing at all.
#[derive(Serialize, Type)]
#[zvariant(signature = "(u(ia{sv}av))")]
struct LayoutReply {
    revision: u32,
    layout: LayoutNode,
}

/// One entry of GetGroupProperties: `(i id, a{sv} properties)`.
#[derive(Serialize, Type)]
#[zvariant(signature = "(ia{sv})")]
struct GroupEntry {
    id: i32,
    props: Dict<'static, 'static>,
}

/// GetGroupProperties with no ids means "every node", per the dbusmenu spec.
fn group_entries(ids: &[i32], only: &[String]) -> Vec<GroupEntry> {
    let ids: Vec<i32> = if ids.is_empty() {
        vec![0, MENU_SHOW, MENU_QUIT]
    } else {
        ids.to_vec()
    };
    ids.into_iter()
        .map(|id| GroupEntry {
            id,
            props: node_props(id, id == 0, only),
        })
        .collect()
}

/// One property as `GetProperty` returns it (`v`). Hosts read `enabled` as a
/// boolean, so lying about the type (an empty string for everything) makes
/// libdbusmenu hosts render a disabled or blank item.
fn menu_property(id: i32, name: &str) -> Value<'static> {
    match name {
        "label" => Value::Str(menu_label(id).into()),
        "enabled" => Value::Bool(true),
        "children-display" if id == 0 => Value::Str("submenu".into()),
        // Unknown or inapplicable property: dbusmenu expects an empty variant;
        // an empty string is the closest `Value` can express.
        _ => Value::Str("".into()),
    }
}

/// What a host event asks the app to do. Ubuntu's AppIndicator and KDE send
/// `clicked`; `activate` is the optional spec event some hosts use instead —
/// reacting only to `activate` makes every menu entry a no-op on stock Ubuntu.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Show,
    Quit,
}

fn menu_action(event_id: &str, id: i32) -> Option<Action> {
    if event_id != "clicked" && event_id != "activate" {
        return None;
    }
    match id {
        MENU_SHOW => Some(Action::Show),
        MENU_QUIT => Some(Action::Quit),
        _ => None,
    }
}

#[interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        property_names: Vec<String>,
    ) -> LayoutReply {
        let layout = if parent_id == 0 {
            root_node(&property_names)
        } else {
            // A host asking for a child layout gets that node, childless.
            LayoutNode {
                id: parent_id,
                props: node_props(parent_id, false, &property_names),
                children: Array::new(&lit_sig("v")),
            }
        };
        LayoutReply {
            revision: next_revision(),
            layout,
        }
    }

    fn get_group_properties(&self, ids: Vec<i32>, property_names: Vec<String>) -> Vec<GroupEntry> {
        group_entries(&ids, &property_names)
    }

    fn get_property(&self, id: i32, name: String) -> Value<'static> {
        menu_property(id, &name)
    }

    /// Nothing changes between opens; `false` = no layout update needed.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    /// Hosts report clicks as `Event(id, "clicked", …)`; see [`menu_action`].
    fn event(&self, id: i32, event_id: &str, _data: Value, _timestamp: u32) {
        match menu_action(event_id, id) {
            Some(Action::Show) => self.shared.show(),
            Some(Action::Quit) => self.shared.quit(),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Menu` property must be a valid D-Bus object path: zbus derives the
    /// property signature (`o`) from the Rust type, and a bad literal would
    /// make the whole interface fail to export.
    #[test]
    fn menu_property_is_a_valid_object_path() {
        let path = ObjectPath::from_static_str(MENU_PATH).expect("valid object path");
        assert_eq!(path.as_str(), "/Menu");
    }

    /// GetGroupProperties must serialize as `a(ia{sv})`; a serde `Value`
    /// inside would silently become `a(iv)` and break every host that asks
    /// for the menu that way.
    #[test]
    fn group_entries_keep_the_dbusmenu_shape() {
        assert_eq!(
            <LayoutReply as Type>::SIGNATURE.to_string(),
            "(u(ia{sv}av))",
            "GetLayout returns one root node, not an array of roots"
        );
        assert_eq!(
            <GroupEntry as Type>::SIGNATURE.to_string(),
            "(ia{sv})",
            "one group entry is (i id, a{{sv}} properties)"
        );
        let all = group_entries(&[], &[]);
        assert_eq!(all.len(), 3, "empty ids means root + both leaves");
        assert_eq!(all[0].id, 0);
    }

    /// Stock Ubuntu's AppIndicator sends `clicked`, not `activate`; handling
    /// only the latter is the bug that made every tray menu entry inert.
    #[test]
    fn both_click_events_reach_the_menu_actions() {
        assert_eq!(menu_action("clicked", MENU_SHOW), Some(Action::Show));
        assert_eq!(menu_action("clicked", MENU_QUIT), Some(Action::Quit));
        assert_eq!(menu_action("activate", MENU_SHOW), Some(Action::Show));
        assert_eq!(menu_action("activate", MENU_QUIT), Some(Action::Quit));
        assert_eq!(menu_action("hovered", MENU_SHOW), None);
        assert_eq!(menu_action("clicked", 99), None);
    }

    /// Property types matter: libdbusmenu reads `enabled` as a boolean and
    /// an empty string makes the item look broken. `only` filtering must
    /// also actually drop keys.
    #[test]
    fn menu_properties_have_real_types_and_honour_filters() {
        assert!(matches!(
            menu_property(MENU_QUIT, "enabled"),
            Value::Bool(true)
        ));
        assert!(
            matches!(menu_property(MENU_SHOW, "label"), Value::Str(label) if !label.is_empty()),
            "the visible label must never be empty"
        );
        assert_eq!(
            menu_property(MENU_QUIT, "children-display"),
            Value::Str("".into()),
            "leaves have no children-display"
        );
        let label_only = group_entries(&[MENU_QUIT], &["label".to_string()]);
        assert_eq!(label_only.len(), 1);
        assert_eq!(label_only[0].props.iter().count(), 1);
    }
}
