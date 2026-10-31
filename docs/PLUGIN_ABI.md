# 可选 cdylib 插件 ABI

> 默认仍是**独立进程 + stdio/MessagePack**。cdylib 是给受信任第一方插件/开发调试的
> 可选模式，崩溃或内存错误会直接带走 Host 进程。

## 1. manifest

```json
{
  "id": "abi-fake",
  "version": "0.1.0",
  "api": 1,
  "kind": "cdylib",
  "protocols": ["http"],
  "capabilities": ["network", "filesystem:task"],
  "executable": { "linux-x86_64": "bin/libaether_plugin_example.so" }
}
```

- `kind` 默认 `process`。
- `executable` 指向 `.so` / `.dylib` / `.dll`；`resolve_executable` 仍做目录逃逸检查。
- `capabilities` 与进程插件同一契约：cdylib 在 Hello 里上报的能力必须与 manifest
  集合相等，否则 Host 拒绝握手（多报/少报都不行）。

## 2. C ABI

动态库导出：

```c
const AetherPluginApi *aether_plugin_api(void);
```

`AetherPluginApi`（`#[repr(C)]`，版本 1）：

| 字段 | 作用 |
| --- | --- |
| `abi_version` | 必须等于 Host 的 `ABI_VERSION` |
| `manifest_json` | 返回 NUL 结尾的 `plugin.json` |
| `create(config)` | 创建插件实例（当前忽略 config） |
| `destroy(handle)` | 销毁实例 |
| `send(handle, frame, len)` | 投递一个完整 IPC 帧（u32 + MessagePack） |
| `poll(handle, out, len, timeout_ms)` | 取一个完整事件帧；1=有，0=超时，-1=错误 |
| `free_buffer(ptr, len)` | 释放 poll 返回的缓冲区 |

帧格式与进程 IPC 完全一致，因此协议类型、Handshake、Progress、Completed 都不变。

## 3. Rust 实现

`aether-plugin-abi` 提供：

- `AbiHandle`：命令队列 + 事件队列 + 工作线程
- `ChannelWriter`：把 `ipc::write_frame` 的字节流拆成完整帧
- `export_abi_plugin!(PluginType, MANIFEST)` 宏

示例：`crates/aether-plugin-cdylib-fake`。

```rust
aether_plugin_abi::export_abi_plugin!(MyPlugin, MANIFEST);
```

## 4. Host 加载

`aether_host::PluginManager` 根据 `manifest.kind` 选择：

- `process`：`PluginProcess`（spawn + stdio）
- `cdylib`：`libloading::Library` + `AbiPlugin`（同进程）

`AbiPlugin` 执行同一套 Hello/Init/Ready 握手；`send/poll` 走 C ABI。

## 5. 安全/稳定性

- 不做 ABI 稳定性承诺给第三方；`abi_version` 不匹配直接拒绝。
- cdylib 崩溃、死锁、内存破坏会影响 Host；进程插件仍是默认。
- 不要加载未签名/来源不明的 cdylib。
- Host 端仍需做 staging 路径校验和 SHA-256 最终校验。
- 未来若要 cdylib 沙箱，需要把 ABI 调用移入独立地址空间，等同于回到进程模型。

## 6. 测试

```bash
cargo build -p aether-plugin-cdylib-fake
cargo test -p aether-host --test runtime
```

测试会加载 `libaether_plugin_cdylib_fake.so`，完成 Probe → Start → Progress →
Completed → finalize。
