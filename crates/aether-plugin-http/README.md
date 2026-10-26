# aether-plugin-http

First-party HTTP/HTTPS download plugin.

## Implemented

- Probe with `HEAD`, falling back to a one-byte `GET` when `HEAD` is rejected.
- Filename from `Content-Disposition` (including RFC 5987 `filename*`) or URL.
- Segmented `Range` downloads with up to 16 connections (minimum 64 KiB per
  connection).
- Resume from `.aether-http-state.json` in the Host-granted staging directory;
  ETag/size/URL changes invalidate the state.
- Pause/resume/cancel without killing the plugin process.
- Streaming SHA-256 verification (optional expected checksum from the Host).
- Writes only to `TaskPaths.staging`; the Host owns finalization.

## Limitations

- No HTTP/3 yet (`http3` capability is not claimed).
- No authentication UI, cookie jar or browser-cookie import; use task settings
  in a later phase.
- `SetBudget` updates the stored budget but does not resize active ranges yet;
  the next task start uses the new value.
- Servers that advertise `Accept-Ranges: bytes` but return `200` for a `Range`
  request currently fail with `RangeMismatch` instead of restarting single
  stream.
- SHA-256 only; BLAKE3 is reserved in the protocol.

## Test

```bash
cargo test -p aether-plugin-http
```

The integration tests start a local Range-capable HTTP server, download a
512 KiB file over four connections, verify the SHA-256, and resume a task from a
persisted state file starting at byte 100.
