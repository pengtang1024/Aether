# aether-plugin-media

First-party HLS/DASH media plugin.

## Implemented (HLS)

- Master playlist parsing and highest-bandwidth variant selection.
- Media playlists with `#EXTINF`, `#EXT-X-MAP` (fMP4 init segment),
  `#EXT-X-BYTERANGE`, `#EXT-X-MEDIA-SEQUENCE` and `#EXT-X-ENDLIST`.
- AES-128 segment decryption (explicit `IV` or media-sequence IV), PKCS#7.
- Segment concatenation into one staging file, resume state, pause/resume/cancel
  and SHA-256 verification.
- VOD only; live playlists are rejected for now.

## Implemented (DASH)

- Static MPD, first Period, highest-bandwidth Representation.
- `SegmentTemplate` with `$Number$` / `$Time$`, `$RepresentationID$`,
  `$Bandwidth$`, `startNumber`, `timescale`, `presentationTimeOffset` and
  `initialization`.
- `SegmentList` (`Initialization` + `SegmentURL`), `BaseURL` inheritance.

## Not yet

- Live DASH (`type="dynamic"`) and multi-Period timelines.
- Live HLS window refresh.
- Subtitle/alternate audio rendition selection, fMP4 sample encryption.

## Test

```bash
cargo test -p aether-plugin-media
```

The tests serve a master/media playlist and segments from a local HTTP server,
then repeat with AES-128 encrypted segments and an explicit IV.
