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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use slint::ComponentHandle;
use zbus::interface;
use zbus::zvariant::{Array, Dict, Signature, Structure, Type, Value};

/// A signature literal that is correct by construction; `expect` documents
/// that a failure here would be a source-code bug, not runtime input.
fn lit_sig(sig: &str) -> Signature {
    Signature::try_from(sig).expect("static signature literal")
}

struct TrayShared {
    visible: Arc<AtomicBool>,
    window: slint::Weak<crate::AppWindow>,
}

impl TrayShared {
    fn show(&self) {
        self.visible.store(true, Ordering::SeqCst);
        let window = self.window.clone();
        // Tray callbacks arrive on the zbus serving thread.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = window.upgrade() {
                let _ = ui.window().show();
            }
        });
    }

    fn quit(&self) {
        self.visible.store(false, Ordering::SeqCst);
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    }
}

/// Start the tray on a dedicated thread. `None` = no session bus; the
/// reason is logged once and the window stays the only entry point.
pub fn start(window: slint::Weak<crate::AppWindow>) -> Option<()> {
    let shared = Arc::new(TrayShared {
        visible: Arc::new(AtomicBool::new(true)),
        window,
    });
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || run(shared, ready_tx));
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Some(()),
        Ok(Err(error)) => {
            eprintln!("aether: no system tray ({error})");
            None
        }
        Err(_) => None,
    }
}

fn run(shared: Arc<TrayShared>, ready: mpsc::Sender<Result<(), String>>) {
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
                "/Menu",
                Menu {
                    shared: shared.clone(),
                },
            )
        })
        .and_then(|_| conn.object_server().at("/MenuBar", Menu { shared }));
    if let Err(error) = export {
        let _ = ready.send(Err(format!("export: {error}")));
        return;
    }
    if let Err(error) = conn.request_name(service.as_str()) {
        let _ = ready.send(Err(format!("bus name: {error}")));
        return;
    }
    // Best effort: without a StatusNotifierWatcher the item is simply not
    // displayed — the "no host" degradation, not an error here.
    let _ = register_with_watcher(&conn, &service);
    let _ = ready.send(Ok(()));
    // The internal zbus thread serves calls; this thread only owns the
    // connection (dropping it releases the bus name).
    loop {
        thread::park();
    }
}

fn register_with_watcher(conn: &zbus::blocking::Connection, service: &str) -> Result<(), String> {
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )
    .map_err(|error| error.to_string())?;
    proxy
        .call_method("RegisterStatusNotifierItem", &(service,))
        .map_err(|error| error.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// org.kde.StatusNotifierItem
// ---------------------------------------------------------------------------

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

    #[zbus(property)]
    fn title(&self) -> &str {
        "Aether 下载管理器"
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "aether"
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

fn node_props(id: i32, root: bool) -> Dict<'static, 'static> {
    let mut props = Dict::new(&lit_sig("s"), &lit_sig("v"));
    let label = match id {
        0 => "Aether",
        MENU_SHOW => "显示主窗口",
        MENU_QUIT => "退出",
        _ => "",
    };
    props
        .append(
            Value::Str("label".into()),
            Value::Value(Box::new(Value::Str(label.into()))),
        )
        .expect("a{sv} key/value");
    props
        .append(
            Value::Str("enabled".into()),
            Value::Value(Box::new(Value::Bool(true))),
        )
        .expect("a{sv} key/value");
    if root {
        props
            .append(
                Value::Str("children-display".into()),
                Value::Value(Box::new(Value::Str("submenu".into()))),
            )
            .expect("a{sv} key/value");
    }
    props
}

/// One menu item as a leaf node: D-Bus `(ia{sv}av)` with empty children.
fn leaf(id: i32) -> Value<'static> {
    Value::Structure(Structure::from((
        Value::I32(id),
        Value::Dict(node_props(id, false)),
        Value::Array(Array::new(&lit_sig("v"))),
    )))
}

/// The menu root: `(0, {children-display: "submenu"}, av[leaf, leaf])`.
fn root_node() -> LayoutNode {
    let mut array = Array::new(&lit_sig("v"));
    for id in [MENU_SHOW, MENU_QUIT] {
        array
            .append(Value::Value(Box::new(leaf(id))))
            .expect("av element");
    }
    LayoutNode {
        id: 0,
        props: node_props(0, true),
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

/// GetLayout's out-args: `u32 revision, a(ia{sv}av) layout`.
#[derive(Serialize, Type)]
#[zvariant(signature = "(ua(ia{sv}av))")]
struct LayoutReply {
    revision: u32,
    layout: Vec<LayoutNode>,
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
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> LayoutReply {
        LayoutReply {
            revision: 1,
            layout: vec![root_node()],
        }
    }

    fn get_property(&self, _id: i32, _name: String) -> Value<'static> {
        Value::Value(Box::new(Value::Str("".into())))
    }

    /// Hosts report menu activation as `Event(id, "activate", …)`.
    fn event(&self, id: i32, event_id: &str, _data: Value, _timestamp: u32) {
        if event_id == "activate" {
            match id {
                MENU_SHOW => self.shared.show(),
                MENU_QUIT => self.shared.quit(),
                _ => {}
            }
        }
    }
}
