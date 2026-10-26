# aether-plugin-sdk

Shared protocol + SDK for Aether first-party download plugins. The Host and
every plugin use this crate so the wire contract is defined once.

## Layout

| Module | Contents |
| --- | --- |
| `manifest` | `plugin.json` parsing, id/version validation, host range, executable resolution |
| `protocol` | `HostCommand`, `PluginEvent`, task/paths/budget/proxy/error types |
| `ipc` | `u32` big-endian length prefix + MessagePack, 8 MiB frame limit |
| `plugin` | `Plugin` trait + `run_stdio` handshake/command loop for plugin binaries |
| `supervisor` | host-side `PluginProcess`: spawn, Hello/Init/Ready handshake, send/receive, shutdown |

## Wire contract

```
plugin → host: Hello { api, plugin_id, version, capabilities }
host → plugin: Init { host_version, data_dir, log_level, proxy, limits }
plugin → host: Ready { api }
```

- `API_VERSION` is `1`; a manifest with a different `api` is rejected.
- Frames are MessagePack maps with a `type` tag, prefixed by a `u32` big-endian
  length. The maximum payload is 8 MiB.
- File bytes never travel over IPC. The plugin writes to `TaskPaths.staging`;
  the Host validates and moves files into `final_dir`.
- `request_id` correlates `Probe`/`Snapshot`/`Ping`; progress uses
  `task_id` + `seq`.

## Writing a plugin

```rust
use aether_plugin_sdk::{run_stdio, Capability, HostCommand, Plugin, PluginContext, SdkError};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }
    fn version(&self) -> &str { "0.1.0" }
    fn capabilities(&self) -> &[Capability] { &[Capability::Network] }

    fn on_command(&mut self, _ctx: &mut PluginContext, command: HostCommand) -> Result<(), SdkError> {
        match command {
            HostCommand::Probe { .. } => { /* send ProbeResult */ }
            HostCommand::Start { .. } => { /* download into paths.staging */ }
            _ => {}
        }
        Ok(())
    }
}

fn main() {
    run_stdio(MyPlugin).expect("plugin loop");
}
```

`run_stdio` sends `Hello`, waits for `Init`, calls `on_init`, sends `Ready`,
handles `Ping`/`Shutdown`, and dispatches everything else to `on_command`.

## Host usage

```rust
let plugin = PluginProcess::spawn(manifest, &executable, init)?;
let probe = plugin.probe("https://example.com/file.bin")?;
plugin.start_task(task, paths, Budget::default())?;
let event = plugin.recv_event(Duration::from_secs(10))?;
plugin.shutdown()?;
```

## Tests

```bash
cargo test --workspace
cargo run -p aether-plugin-fake   # speaks the protocol on stdin/stdout
```

The fake plugin is a real child process in the contract tests: it performs the
handshake, answers `Probe`, writes a 1 KiB file into staging and reports
`Progress`/`Completed`.

## Status

P0: protocol, framing, supervisor and fake plugin are implemented and tested.
The Host UI is not wired yet; the HTTP plugin is P1.
