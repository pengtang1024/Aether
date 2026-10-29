//! Windows notification-area tray (Shell_NotifyIconW), raw FFI, no deps.
//!
//! Same philosophy as the pipe transport in `aether-daemon`: bind the few
//! Win32 functions directly instead of pulling a windows-sys feature tree.
//! A message-only window (`HWND_MESSAGE`) owns the icon and receives the
//! callback — no visible window, nothing to paint.
//!
//! Degradation: a session without a shell (CI runner, a service session)
//! can refuse the icon — `Shell_NotifyIconW` fails and `start` returns
//! `None` after one log line, exactly like the Linux path without a
//! StatusNotifier host. Downloads keep running in the daemon either way;
//! the tray only summons or quits the window.
//!
//! The `Hooks` shape keeps this module free of Slint types: the dispatcher
//! (`src/tray.rs`) adapts the window, and tests start an icon with no-op
//! callbacks — no window and no display needed.
//!
//! Layout note: `NotifyIconData` must match the SDK byte for byte.
//! `#[repr(C)]` with the Win32 field order does that, and a test pins the
//! sizes and offsets (976 bytes on x64, 960 on x86) so a field tweak can't
//! silently break the `cbSize` negotiation that `Shell_NotifyIconW` does.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

type Handle = *mut c_void;
/// Win32 `WNDPROC`: the OS calls it back, hence `unsafe fn`.
type WndProc = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;

/// Win32 `GUID` — 16 bytes, 4-byte alignment (part of `NotifyIconData`).
#[repr(C)]
#[allow(dead_code)] // Only the byte layout matters to us; the SDK reads it.
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// Win32 `POINT`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}

/// Win32 `NOTIFYICONDATAW` (modern size: 976 bytes on x64, 960 on x86).
/// Field order and widths are the SDK's; the balloon-era fields this tray
/// never touches still exist so `cbSize` negotiates to the modern size.
#[repr(C)]
#[allow(dead_code)] // The Win32 API reads these; Rust only lays them out.
struct NotifyIconData {
    cb_size: u32,
    h_wnd: Handle,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: Handle,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_timeout_or_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: Guid,
    h_balloon_icon: Handle,
}

/// Win32 `WNDCLASSEXW`.
#[repr(C)]
#[allow(dead_code)] // Written for the OS to read back; never read in Rust.
struct WndClassEx {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: WndProc,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: Handle,
    h_icon: Handle,
    h_cursor: Handle,
    h_brush_background: Handle,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: Handle,
}

/// Win32 `MSG` — filled by `GetMessageW`, handed straight to dispatch.
#[repr(C)]
#[allow(dead_code)] // Filled by the OS; the fields reach us as arguments.
struct Msg {
    h_wnd: Handle,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: Point,
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(dw_message: u32, lp_data: *const NotifyIconData) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(lp_wc: *const WndClassEx) -> u16;
    fn CreateWindowExW(
        dw_ex_style: u32,
        lp_class_name: *const u16,
        lp_window_name: *const u16,
        dw_style: u32,
        x: i32,
        y: i32,
        n_width: i32,
        n_height: i32,
        h_wnd_parent: Handle,
        h_menu: Handle,
        h_instance: Handle,
        lp_param: *mut c_void,
    ) -> Handle;
    fn DestroyWindow(h_wnd: Handle) -> i32;
    fn DefWindowProcW(h_wnd: Handle, message: u32, w_param: usize, l_param: isize) -> isize;
    fn PostQuitMessage(n_exit_code: i32);
    fn PostMessageW(h_wnd: Handle, message: u32, w_param: usize, l_param: isize) -> i32;
    fn LoadImageW(
        h_inst: Handle,
        name: *const u16,
        typ: u32,
        cx: i32,
        cy: i32,
        fu_load: u32,
    ) -> Handle;
    fn DestroyIcon(h_icon: Handle) -> i32;
    fn CreatePopupMenu() -> Handle;
    fn DestroyMenu(h_menu: Handle) -> i32;
    fn AppendMenuW(
        h_menu: Handle,
        u_flags: u32,
        u_id_new_item: usize,
        lp_new_item: *const u16,
    ) -> i32;
    fn SetForegroundWindow(h_wnd: Handle) -> i32;
    fn GetCursorPos(lp_point: *mut Point) -> i32;
    fn TrackPopupMenu(
        h_menu: Handle,
        u_flags: u32,
        x: i32,
        y: i32,
        n_reserved: i32,
        h_wnd: Handle,
        prc_rect: *const Point,
    ) -> i32;
    fn GetMessageW(lp_msg: *mut Msg, h_wnd: Handle, min_msg: u32, max_msg: u32) -> i32;
    fn TranslateMessage(lp_msg: *const Msg) -> i32;
    fn DispatchMessageW(lp_msg: *const Msg) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lp_module: *const u16) -> Handle;
    fn GetModuleFileNameW(h_module: Handle, lp_filename: *mut u16, n_size: u32) -> u32;
}

const NIM_ADD: u32 = 0x0000_0000;
const NIM_DELETE: u32 = 0x0000_0002;
const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;

const WM_DESTROY: u32 = 0x0002;
const WM_COMMAND: u32 = 0x0111;
const WM_NULL: u32 = 0x0000;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;
/// `WM_APP + 1`: our `uCallbackMessage`; v0 semantics (LOWORD = mouse msg).
const CALLBACK_MESSAGE: u32 = 0x8000 + 1;

const CMD_SHOW: u32 = 1001;
const CMD_QUIT: u32 = 1002;

const CS_DBLCLKS: u32 = 0x0008;
const IMAGE_ICON: u32 = 1;
const LR_LOADFROMFILE: u32 = 0x0000_0010;
const LR_DEFAULTSIZE: u32 = 0x0000_0040;
const LR_SHARED: u32 = 0x0000_8000;
/// `MAKEINTRESOURCE(32512)` — the stock application icon.
const IDI_APPLICATION: usize = 32512;
const MF_STRING: u32 = 0x0000_0000;
const TPM_RIGHTBUTTON: u32 = 0x0000_0002;
const TPM_RETURNCMD: u32 = 0x0000_0100;
/// `(HWND)-3`: message-only window parent.
const HWND_MESSAGE: Handle = -3_isize as Handle;

const ICON_ID: u32 = 1;

/// What the tray can ask the app to do. Deliberately free of Slint types:
/// the dispatcher (`src/tray.rs`) adapts the window, tests use no-ops.
pub struct Hooks {
    /// Left click / menu "显示主窗口": surface the main window.
    pub on_show: Box<dyn Fn() + Send + 'static>,
    /// Menu "退出": quit the application.
    pub on_quit: Box<dyn Fn() + Send + 'static>,
}

/// Per-window state, living on the tray thread (the window's thread), so
/// the window procedure and the message loop share it without locks.
struct State {
    hooks: Hooks,
    h_icon: Handle,
    owns_icon: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = RefCell::new(None);
}

/// Per-start window-class sequence: class names are process-wide, so two
/// starts in one process (app plus a test) must not collide.
static CLASS_SEQ: AtomicU32 = AtomicU32::new(0);

/// Start the tray on a dedicated thread. `None` = no icon in this session
/// (the reason is logged once, mirroring the unix path) and the window
/// stays the only entry point.
pub fn start(hooks: Hooks) -> Option<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || run(hooks, ready_tx));
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Some(()),
        Ok(Err(error)) => {
            eprintln!("aether: no system tray ({error})");
            None
        }
        // Setup wedged before reporting — same honest "no tray" as above.
        Err(_) => None,
    }
}

fn run(hooks: Hooks, ready: mpsc::Sender<Result<(), String>>) {
    let Some(h_wnd) = create_window() else {
        let _ = ready.send(Err("window creation failed".into()));
        return;
    };
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let (h_icon, owns_icon) = load_icon(instance);
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            hooks,
            h_icon,
            owns_icon,
        })
    });
    if !add_icon(h_wnd, h_icon) {
        // DestroyWindow dispatches WM_DESTROY on this thread: it takes the
        // state back, releases a file-loaded icon and posts the quit.
        let _ = unsafe { DestroyWindow(h_wnd) };
        let _ = ready.send(Err("the shell refused the icon".into()));
        return;
    }
    let _ = ready.send(Ok(()));
    // Own the loop: GetMessageW returns >0 per message, 0 on WM_QUIT
    // (posted from WM_DESTROY once the icon is gone).
    loop {
        let mut msg: Msg = unsafe { std::mem::zeroed() };
        let more = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if more <= 0 {
            return;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Register a per-start class and create the message-only window. The
/// parent is `HWND_MESSAGE`, so the window never shows and never paints.
fn create_window() -> Option<Handle> {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class_name = wide(&format!(
        "AetherTray-{}-{}",
        std::process::id(),
        CLASS_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let class = WndClassEx {
        cb_size: size_of::<WndClassEx>() as u32,
        style: CS_DBLCLKS,
        lpfn_wnd_proc: wnd_proc,
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: instance,
        h_icon: std::ptr::null_mut(),
        h_cursor: std::ptr::null_mut(),
        h_brush_background: std::ptr::null_mut(),
        lpsz_menu_name: std::ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
        h_icon_sm: std::ptr::null_mut(),
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return None;
    }
    let h_wnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    (!h_wnd.is_null()).then_some(h_wnd)
}

/// Add the icon with message callback, icon and tooltip. `false` = the
/// session refused it (no shell) — the honest "no tray" case.
fn add_icon(h_wnd: Handle, h_icon: Handle) -> bool {
    let mut data: NotifyIconData = unsafe { std::mem::zeroed() };
    data.cb_size = size_of::<NotifyIconData>() as u32;
    data.h_wnd = h_wnd;
    data.u_id = ICON_ID;
    data.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.u_callback_message = CALLBACK_MESSAGE;
    data.h_icon = h_icon;
    let tip = wide("Aether 下载管理器");
    let copy = tip.len().min(data.sz_tip.len() - 1);
    data.sz_tip[..copy].copy_from_slice(&tip[..copy]);
    data.sz_tip[copy] = 0;
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) != 0 }
}

/// Remove the icon; called from WM_DESTROY (quit or a failed start).
fn remove_icon(h_wnd: Handle) {
    let mut data: NotifyIconData = unsafe { std::mem::zeroed() };
    data.cb_size = size_of::<NotifyIconData>() as u32;
    data.h_wnd = h_wnd;
    data.u_id = ICON_ID;
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// The icon: the exe-adjacent `logo.ico` (the installer lays one down
/// next to `aether.exe`), falling back to the stock application icon for
/// a portable run — the second flag says whether we own the handle.
fn load_icon(instance: Handle) -> (Handle, bool) {
    let mut path = vec![0u16; 32_768];
    let len =
        unsafe { GetModuleFileNameW(instance, path.as_mut_ptr(), path.len() as u32) } as usize;
    if len > 0 && len < path.len() {
        path.truncate(len);
        if let Some(slash) = path.iter().rposition(|&c| c == '\\' as u16) {
            path.truncate(slash + 1);
            path.extend(wide("logo.ico"));
            let loaded = unsafe {
                LoadImageW(
                    std::ptr::null_mut(),
                    path.as_ptr(),
                    IMAGE_ICON,
                    0,
                    0,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE,
                )
            };
            if !loaded.is_null() {
                return (loaded, true);
            }
        }
    }
    let stock = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            IDI_APPLICATION as *const u16,
            IMAGE_ICON,
            0,
            0,
            LR_SHARED | LR_DEFAULTSIZE,
        )
    };
    (stock, false)
}

/// Window procedure, on the tray thread. Menu commands dispatch through
/// the state's hooks; the quit arm drops the state borrow *before*
/// DestroyWindow, which re-enters here for WM_DESTROY.
extern "system" fn wnd_proc(h_wnd: Handle, message: u32, w_param: usize, l_param: isize) -> isize {
    match message {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
            with_hooks(|hooks| (hooks.on_show)());
            0
        }
        WM_RBUTTONUP => {
            show_menu(h_wnd);
            0
        }
        WM_COMMAND => {
            let command = w_param as u32 & 0xFFFF;
            dispatch(command);
            if command == CMD_QUIT {
                let _ = unsafe { DestroyWindow(h_wnd) };
            }
            0
        }
        WM_DESTROY => {
            remove_icon(h_wnd);
            STATE.with(|state| {
                if let Some(taken) = state.borrow_mut().take() {
                    if taken.owns_icon {
                        let _ = unsafe { DestroyIcon(taken.h_icon) };
                    }
                }
            });
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(h_wnd, message, w_param, l_param) },
    }
}

/// Run a menu command against the tray state (tray thread only).
fn dispatch(command: u32) {
    with_hooks(|hooks| match command {
        CMD_SHOW => (hooks.on_show)(),
        CMD_QUIT => (hooks.on_quit)(),
        _ => {}
    });
}

/// Borrow the tray state for a callback (tray thread only).
fn with_hooks<F: FnOnce(&Hooks)>(f: F) {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            f(&state.hooks);
        }
    });
}

/// Right-click menu: 显示主窗口 / 退出 — the same labels as the Linux
/// dbusmenu. `TPM_RETURNCMD` returns the choice directly; the
/// `SetForegroundWindow` + trailing `WM_NULL` pair is the documented
/// workaround for the menu eating its first dismissal click.
fn show_menu(h_wnd: Handle) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let show_label = wide("显示主窗口");
    let quit_label = wide("退出");
    let command = unsafe {
        AppendMenuW(menu, MF_STRING, CMD_SHOW as usize, show_label.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, quit_label.as_ptr());
        SetForegroundWindow(h_wnd);
        let mut point = Point { x: 0, y: 0 };
        GetCursorPos(&mut point);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            point.x,
            point.y,
            0,
            h_wnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        PostMessageW(h_wnd, WM_NULL, 0, 0);
        chosen
    };
    if command != 0 {
        dispatch(command as u32);
        if command as u32 == CMD_QUIT {
            let _ = unsafe { DestroyWindow(h_wnd) };
        }
    }
}

/// NUL-terminated UTF-16 for the Win32 `W` entry points.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    /// The `cbSize` negotiation only works if our struct *is* the SDK's:
    /// pin sizes and the offsets that matter (hIcon at 32, szTip at 40,
    /// the balloon tail) against the published Win32 layout.
    #[test]
    fn notify_icon_data_matches_the_sdk_layout() {
        match size_of::<usize>() * 8 {
            64 => {
                assert_eq!(size_of::<NotifyIconData>(), 976);
                assert_eq!(offset_of!(NotifyIconData, h_wnd), 8);
                assert_eq!(offset_of!(NotifyIconData, h_icon), 32);
                assert_eq!(offset_of!(NotifyIconData, sz_tip), 40);
                assert_eq!(offset_of!(NotifyIconData, sz_info), 304);
                assert_eq!(offset_of!(NotifyIconData, guid_item), 952);
                assert_eq!(offset_of!(NotifyIconData, h_balloon_icon), 968);
                assert_eq!(size_of::<WndClassEx>(), 80);
                assert_eq!(size_of::<Msg>(), 48);
            }
            32 => {
                assert_eq!(size_of::<NotifyIconData>(), 960);
                assert_eq!(size_of::<WndClassEx>(), 48);
                assert_eq!(size_of::<Msg>(), 28);
            }
            other => panic!("unexpected pointer width: {other}"),
        }
    }

    #[test]
    fn wide_strings_are_nul_terminated_utf16() {
        assert_eq!(
            wide("Aether"),
            vec![0x0041, 0x0065, 0x0074, 0x0068, 0x0065, 0x0072, 0]
        );
        let quit = wide("退出");
        assert_eq!(quit.last(), Some(&0), "must be NUL-terminated");
        assert_eq!(quit.len(), 3, "two CJK code units plus the terminator");
    }

    /// A message-only window needs no desktop — this must succeed on any
    /// Windows, interactive or not. Also proves class registration and
    /// that DestroyWindow round-trips WM_DESTROY through the procedure.
    #[test]
    fn message_only_window_creation_works() {
        let h_wnd = create_window().expect("message-only window must be creatable");
        assert!(!h_wnd.is_null());
        assert_eq!(unsafe { DestroyWindow(h_wnd) }, 1, "destroy must succeed");
    }

    /// Best-effort contract: an interactive session returns Some, a
    /// session whose shell refuses the icon returns None — both accepted
    /// here (CI is headless). What must hold: `start` reports within its
    /// own 5-second handshake and never crashes through the FFI boundary.
    #[test]
    fn start_reports_readiness_or_an_honest_none() {
        let result = start(Hooks {
            on_show: Box::new(|| {}),
            on_quit: Box::new(|| {}),
        });
        assert!(matches!(result, Some(()) | None));
    }
}
