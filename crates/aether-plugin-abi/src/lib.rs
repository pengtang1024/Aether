//! Optional `cdylib` plugin ABI.
//!
//! The default plugin model remains a separate process. A cdylib plugin runs
//! inside the Host process, so a crash or memory bug takes down the Host. Use
//! it only for trusted, first-party or development plugins.
//!
//! Wire format: the same length-prefixed MessagePack frames as the process
//! IPC. `send` accepts one complete frame, `poll` returns one complete frame.

use std::ffi::c_void;
use std::io::Write;
use std::os::raw::c_char;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use aether_plugin_sdk::{ipc, HostCommand, Init, Plugin, PluginContext, PluginEvent, API_VERSION};

pub const ABI_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AetherPluginApi {
    pub abi_version: u32,
    pub manifest_json: unsafe extern "C" fn() -> *const c_char,
    pub create: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub send: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
    pub poll: unsafe extern "C" fn(*mut c_void, *mut *mut u8, *mut usize, u32) -> i32,
    pub free_buffer: unsafe extern "C" fn(*mut u8, usize),
}

/// Host-side handle around one in-process plugin instance.
pub struct AbiHandle {
    commands: Sender<HostCommand>,
    events: Receiver<Vec<u8>>,
}

impl AbiHandle {
    pub fn spawn<P: Plugin + 'static>(plugin: P) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        thread::spawn(move || run_loop(plugin, command_rx, event_tx));
        Self { commands, events }
    }

    pub fn send_frame(&self, frame: &[u8]) -> Result<(), String> {
        let command = ipc::decode_frame(frame).map_err(|error| error.to_string())?;
        self.commands
            .send(command)
            .map_err(|_| "plugin command loop is closed".to_string())
    }

    pub fn poll(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.events.recv_timeout(timeout).ok()
    }
}

fn run_loop<P: Plugin>(mut plugin: P, commands: Receiver<HostCommand>, events: Sender<Vec<u8>>) {
    let mut ctx = PluginContext::new(Box::new(ChannelWriter::new(events)));

    if ctx
        .send(&PluginEvent::Hello {
            api: API_VERSION,
            plugin_id: plugin.id().to_string(),
            version: plugin.version().to_string(),
            capabilities: plugin.capabilities().to_vec(),
        })
        .is_err()
    {
        return;
    }

    let init = match commands.recv() {
        Ok(HostCommand::Init {
            host_version,
            data_dir,
            log_level,
            proxy,
            limits,
            network,
        }) => Init {
            host_version,
            data_dir,
            log_level,
            proxy,
            limits,
            network,
        },
        _ => return,
    };
    if plugin.on_init(&mut ctx, &init).is_err()
        || ctx.send(&PluginEvent::Ready { api: API_VERSION }).is_err()
    {
        return;
    }

    while let Ok(command) = commands.recv() {
        let result = match command {
            HostCommand::Ping { request_id } => ctx.send(&PluginEvent::Pong {
                request_id,
                unix_ms: now_ms(),
            }),
            HostCommand::Shutdown => {
                let _ = plugin.on_shutdown(&mut ctx);
                break;
            }
            HostCommand::Init { .. } => break,
            other => plugin.on_command(&mut ctx, other),
        };
        if result.is_err() {
            break;
        }
    }
}

/// Split arbitrary writes into complete length-prefixed frames.
struct ChannelWriter {
    sender: Sender<Vec<u8>>,
    buffer: Vec<u8>,
}

impl ChannelWriter {
    fn new(sender: Sender<Vec<u8>>) -> Self {
        Self {
            sender,
            buffer: Vec::new(),
        }
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(data);
        loop {
            if self.buffer.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]) as usize;
            let frame_len = 4 + len;
            if self.buffer.len() < frame_len {
                break;
            }
            let frame = self.buffer.drain(..frame_len).collect::<Vec<_>>();
            if self.sender.send(frame).is_err() {
                break;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Export the C ABI for a plugin type.
///
/// `$manifest` must be a string literal containing `plugin.json` with
/// `"kind": "cdylib"`.
#[macro_export]
macro_rules! export_abi_plugin {
    ($plugin:expr, $manifest:expr) => {
        unsafe extern "C" fn __aether_manifest() -> *const ::std::os::raw::c_char {
            static AETHER_MANIFEST_CACHE: ::std::sync::OnceLock<::std::ffi::CString> =
                ::std::sync::OnceLock::new();
            AETHER_MANIFEST_CACHE
                .get_or_init(|| {
                    ::std::ffi::CString::new($manifest)
                        .expect("plugin manifest must not contain NUL")
                })
                .as_ptr()
        }

        unsafe extern "C" fn __aether_create(
            _config: *const ::std::os::raw::c_char,
        ) -> *mut ::std::ffi::c_void {
            ::std::boxed::Box::into_raw(::std::boxed::Box::new($crate::AbiHandle::spawn($plugin)))
                as *mut _
        }

        unsafe extern "C" fn __aether_destroy(handle: *mut ::std::ffi::c_void) {
            if !handle.is_null() {
                ::core::mem::drop(::std::boxed::Box::from_raw(
                    handle as *mut $crate::AbiHandle,
                ));
            }
        }

        unsafe extern "C" fn __aether_send(
            handle: *mut ::std::ffi::c_void,
            data: *const u8,
            len: usize,
        ) -> i32 {
            if handle.is_null() {
                return -1;
            }
            let bytes = ::std::slice::from_raw_parts(data, len);
            match (&*(handle as *const $crate::AbiHandle)).send_frame(bytes) {
                Ok(()) => 0,
                Err(_) => -1,
            }
        }

        unsafe extern "C" fn __aether_poll(
            handle: *mut ::std::ffi::c_void,
            out: *mut *mut u8,
            out_len: *mut usize,
            timeout_ms: u32,
        ) -> i32 {
            if handle.is_null() || out.is_null() || out_len.is_null() {
                return -1;
            }
            match (&*(handle as *const $crate::AbiHandle))
                .poll(::std::time::Duration::from_millis(timeout_ms as u64))
            {
                Some(frame) => {
                    let boxed = frame.into_boxed_slice();
                    *out_len = boxed.len();
                    *out = ::std::boxed::Box::into_raw(boxed) as *mut u8;
                    1
                }
                None => 0,
            }
        }

        unsafe extern "C" fn __aether_free(data: *mut u8, len: usize) {
            if !data.is_null() {
                ::core::mem::drop(::std::boxed::Box::from_raw(
                    ::std::ptr::slice_from_raw_parts_mut(data, len),
                ));
            }
        }

        #[no_mangle]
        pub extern "C" fn aether_plugin_api() -> *const $crate::AetherPluginApi {
            static API: $crate::AetherPluginApi = $crate::AetherPluginApi {
                abi_version: $crate::ABI_VERSION,
                manifest_json: __aether_manifest,
                create: __aether_create,
                destroy: __aether_destroy,
                send: __aether_send,
                poll: __aether_poll,
                free_buffer: __aether_free,
            };
            &API
        }
    };
}
