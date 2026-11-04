//! Daemon event loop and Unix-socket server.
//!
//! The implementation lives in submodules. They reach this prelude through
//! `use super::*`, so the import list below is their shared vocabulary — do
//! not trim it without moving the imports into each module.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_host::pac::PacResolver;
use aether_host::system_proxy::{self, SystemProxy, SYSTEM_PROXY_TTL};
use aether_host::{
    default_plugins_dir, ensure_space, finalize_task_with_checksum, human_bytes, NewTask,
    PluginEventEnvelope, PluginManager, PluginRegistry, ProxySetting, StoredTask, TaskState,
    TaskStore,
};
use aether_plugin_sdk::{
    Budget, Checksum, ChecksumAlgorithm, FileInfo, HostCommand, PluginEvent, PluginKind,
    ProxyConfig, TaskPaths, TaskSpec,
};
use aether_update::UpdateClient;
use fs2::FileExt;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::proto::{Request, Response};
use crate::service::{ServiceBridge, ServiceStatus, ServiceSupervisor};
use crate::settings::{
    parse_hhmm, Limits, Settings, CONNECTIONS_DEFAULT, KEYS, RPC_LISTEN_DEFAULT,
};
use crate::transport::{Conn, Listener};
use crate::volume::same_filesystem;
mod config;
mod events;
mod lifecycle;
mod plugins;
mod proxy;
mod recovery;
mod requests;
mod rpc;
mod run;
mod scheduler;
mod staging;
mod state;
mod tasks;
mod transport;
mod updates;

#[cfg(test)]
mod tests;

pub use config::DaemonConfig;
pub use run::run;
pub(crate) use transport::{subscribe_events, Command};
pub use updates::plugin_auto_update_pass;

pub(crate) use events::*;
pub(crate) use lifecycle::*;
pub(crate) use plugins::updates::*;
pub(crate) use plugins::{install::*, package::*};
pub(crate) use proxy::*;
pub(crate) use recovery::*;
pub(crate) use requests::handle_request;
pub(crate) use rpc::*;
pub(crate) use scheduler::{limits::*, retry::*, window::*};
pub(crate) use staging::*;
pub(crate) use state::*;
pub(crate) use tasks::*;
pub(crate) use transport::*;
pub(crate) use updates::*;
