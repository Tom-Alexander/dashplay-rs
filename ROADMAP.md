# Roadmap

This roadmap tracks the gap between the current implementation and the full player
described in [`ARCHITECTURE.md`](ARCHITECTURE.md).

`dashplayrs` today is a **DASH segment-delivery pipeline**: it parses MPDs, resolves
timelines, applies BOLA ABR, fetches (and optionally Widevine-decrypts) fragmented MP4
init/media segments, and delivers them as `PlayerEvent::{Init, Segment, End}` over
broadcast channels. It is not yet a complete player framework.

Priorities follow the project ordering: **correctness → standards compliance →
reliability → maintainability → performance → API ergonomics**.

Status legend: `[ ]` not started · `[~]` partial · `[x]` done.

---

## Current status

| Area | Status | Notes |
|------|:------:|-------|
| MPD parse (`dash-mpd`) | `[x]` | Parsed; SegmentTemplate inheritance flattened at Period/AS/Rep |
| Timeline / live edge | `[x]` | SegmentTemplate + SegmentTimeline, UTCTiming, TSBD filtering |
| Segment fetch + BaseURL failover | `[x]` | Blacklisting on failure |
| Multi-period (live + VOD) | `[x]` | Init re-emission on period transition |
| ABR (BOLA) | `[x]` | Consumer-reported buffer via [`BufferFeedback`] |
| DRM | `[~]` | Widevine only; requires external CDM device |
| Track selection | `[~]` | MIME type + language/role/codec/accessibility; text, trick-play, and image tracks opt-in |
| Segment addressing | `[~]` | `SegmentTemplate`, `SegmentList` / `SegmentURL`, `SegmentBase` + byte ranges |
| Playback control (seek/pause/stop) | `[x]` | `PlaybackController` with state machine |
| Demux / decode | `[ ]` | Out of scope (bytes only) |
| Metrics / rich events | `[~]` | Per-track [`TrackMetrics`]; fragment + [`MediaEvent`] events |
| Pluggable networking / ABR | `[x]` | HTTP client trait + `ReqwestClient`; ABR trait + `BolaAbrFactory` |

---

## P0 — Correctness and standards foundations

These close the largest gaps between "delivers some streams" and "handles conformant DASH".

- [x] **SegmentTemplate inheritance flattening.** Resolve `SegmentTemplate` declared at
  `Period` and `MPD` level, not just on the `AdaptationSet`. Today `dash_stream.rs`
  hard-requires `AdaptationSet@SegmentTemplate` and errors otherwise.
- [x] **SegmentList addressing.** Support explicit `SegmentList` / `SegmentURL` media
  addressing.
- [x] **SegmentBase + byte ranges.** Support `SegmentBase`, `Initialization@range`, and
  `indexRange` (single-file `sidx`-indexed representations).
- [x] **Full template variable set.** `$Bandwidth$` and width/format specifiers such as
  `$Number%05d$` / `$Time%0Nd$`.
- [x] **Real buffer feedback for ABR.** Replace the synthetic `abr.update_buffer(10.0)`
  seed with a buffer level reported by the consumer, so ABR reflects actual playback
  state rather than download timing alone.
- [x] **Representation fallback on segment failure.** README claims it, but only BaseURL
  failover exists; add fallback to a lower representation when a segment fetch fails.

## P1 — Reliability and playback lifecycle

- [x] **Playback control API.** `seek`, `pause`, `resume`, `stop`, and a `PlaybackState`
  machine (`Idle`, `Buffering`, `Playing`, `Seeking`, `Paused`, `Ended`, `Error`) as promised in
  `ARCHITECTURE.md`.
- [x] **Explicit lifecycle vs. background tasks.** `MediaPlayer::start` prepares playback
  without spawning; callers run the loop via [`PlayerOutputs::run`] or opt into
  [`PlayerOutputs::spawn`]. Parallel adaptation-set fetches use cooperative `join` inside the
  stream controller instead of hidden tasks. [`Player::start_tracks`] documents its single
  spawned background task as the high-level concurrency contract.
- [x] **Downloaded-segment dedup.** Track already-delivered segments across manifest
  refreshes so live refresh cannot re-emit or skip fragments.
- [x] **Static multi-period VOD.** Live multi-period is covered; add VOD multi-period
  handling and tests.
- [x] **License renewal / key rotation.** Handle Widevine key rotation and license
  renewal during long live sessions.

## P2 — Track selection and auxiliary content

- [x] **Rich track selection.** Select by language, role, codecs, and accessibility;
  allow user preferences and multiple audio tracks. Today selection is MIME-type only.
- [x] **Subtitles / captions.** `text/vtt`, TTML, and in-band caption tracks (`stpp`, `wvtt`, `c608`).
- [x] **Thumbnails / trick-play.** Image adaptation sets (`image/jpeg`) and trick-play
  tracks.
- [x] **EssentialProperty / SupplementalProperty.** Respect descriptors for codec/scheme
  compatibility and role signalling.

## P3 — Observability and extensibility

- [x] **Metrics API.** Throughput history, buffer level, startup delay, rebuffer events,
  and bitrate-switch history (feeds ABR without influencing playback directly).
- [x] **Richer event model.** Add `ManifestLoaded`, `BufferUpdated`, `BitrateChanged`,
  `PlaybackStarted`/`Ended`, and error events alongside the current fragment events.
- [x] **Pluggable HTTP client.** Abstract networking behind a trait so `reqwest` is one
  of several backends (WASM/browser fetch, custom TLS, embedded stacks).
- [x] **Pluggable ABR.** Introduce an ABR trait/rules engine; keep BOLA as the default
  implementation.

## P4 — Additional DRM and advanced DASH

- [ ] **Additional DRM systems.** ClearKey
- [~] **Low-Latency DASH.** `availabilityTimeComplete`, `ServiceDescription`, resync
  points, and chunked/partial segment transfer. *(Availability timing, `ServiceDescription`
  target latency, and chunked CMAF partial transfer done; resync points remain.)*
- [x] **In-band and MPD events.** `EventStream`, `emsg`, and SCTE-35 ad markers.
- [ ] **Content steering / MPD updates.** `Location`, content steering, and MPD patch
  (`urn:mpeg:dash:mpd-patch`) updates.
- [ ] **CMCD / CMSD.** Common Media Client/Server Data request and response hints.

## P5 — Conformance and quality

- [x] **DASH-IF conformance suite.** `tests/dashif.rs` covers playback vectors, Rust IOP
  schematron validation (`tests/conformance/iop_validate.rs`, rules from DASH-IF
  Conformance-Software), CENC encrypted local vector (`dashif_drm_encrypted`), optional
  full Widevine playback with `DEVICE_PATH` + captured `license-response.bin`, and remote
  vectors (`cargo test --test dashif -- --ignored`).
- [ ] **Regression tests for each fix.** Per `AGENTS.md`, every bug fix ships with a
  deterministic regression test.
- [x] **API surface cleanup.** Public helpers (`start_merged`, `into_async_read`, track
  subscription helpers) are exported from the crate root and covered by integration tests;
  stale `#[allow(dead_code)]` attributes removed.
