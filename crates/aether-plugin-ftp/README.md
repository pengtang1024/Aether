# aether-plugin-ftp

First-party FTP download plugin.

## Implemented

- `ftp://` and explicit `ftps://` URLs with username/password (percent-decoded)
  and anonymous default; FTPS uses rustls with Mozilla roots.
- Probe with `SIZE`; filename from the URL path.
- Binary-mode `RETR`, resume with `REST` based on the existing staging file
  length.
- Pause/resume/cancel checks while streaming.
- SHA-256 verification and Host-granted staging only.

## Limitations

- FTPS uses the system-independent Mozilla root set; custom CAs/mTLS are not
  supported yet.
- No proxy, active mode, MLSD, or multi-connection segments; FTP servers usually
  serialize one data connection.
- Remote `MDTM` is not checked; a same-size remote change can resume incorrectly.
