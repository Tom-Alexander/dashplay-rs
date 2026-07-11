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
| Multi-period (live) | `[x]` | Init re-emission on transition |
| ABR (BOLA) | `[~]` | Works, but drives off a synthetic buffer estimate |
| DRM | `[~]` | Widevine only; requires external CDM device |
| Track selection | `[~]` | MIME type only (`audio/mp4`, `video/mp4`) |
| Segment addressing | `[~]` | `SegmentTemplate` and `SegmentList` / `SegmentURL` |
| Playback control (seek/pause/stop) | `[ ]` | Not implemented |
| Demux / decode | `[ ]` | Out of scope (bytes only) |
| Metrics / rich events | `[ ]` | Only `Init`/`Segment`/`End` |
| Pluggable networking / ABR | `[ ]` | Hard-wired to `reqwest` / BOLA |

---

## P0 — Correctness and standards foundations

These close the largest gaps between "delivers some streams" and "handles conformant DASH".

- [x] **SegmentTemplate inheritance flattening.** Resolve `SegmentTemplate` declared at
  `Period` and `MPD` level, not just on the `AdaptationSet`. Today `dash_stream.rs`
  hard-requires `AdaptationSet@SegmentTemplate` and errors otherwise.
- [x] **SegmentList addressing.** Support explicit `SegmentList` / `SegmentURL` media
  addressing.
- [ ] **SegmentBase + byte ranges.** Support `SegmentBase`, `Initialization@range`, and
  `indexRange` (single-file `sidx`-indexed representations).
- [ ] **Full template variable set.** `$Bandwidth$` and width/format specifiers such as
  `$Number%05d$` / `$Time%0Nd$`.
- [ ] **Real buffer feedback for ABR.** Replace the synthetic `abr.update_buffer(10.0)`
  seed with a buffer level reported by the consumer, so ABR reflects actual playback
  state rather than download timing alone.
- [ ] **Representation fallback on segment failure.** README claims it, but only BaseURL
  failover exists; add fallback to a lower representation when a segment fetch fails.

## P1 — Reliability and playback lifecycle

- [ ] **Playback control API.** `seek`, `pause`, `resume`, `stop`, and a `PlaybackState`
  machine (`Idle`, `Buffering`, `Playing`, `Seeking`, `Ended`, `Error`) as promised in
  `ARCHITECTURE.md`.
- [ ] **Explicit lifecycle vs. background tasks.** `MediaPlayer::start` and
  `stream_controller` currently spawn hidden `tokio` tasks, which conflicts with the
  architecture principle of never spawning hidden background work. Either make the loop
  caller-driven or document/expose it as an explicit concurrency contract.
- [ ] **Downloaded-segment dedup.** Track already-delivered segments across manifest
  refreshes so live refresh cannot re-emit or skip fragments.
- [ ] **Static multi-period VOD.** Live multi-period is covered; add VOD multi-period
  handling and tests.
- [ ] **License renewal / key rotation.** Handle Widevine key rotation and license
  renewal during long live sessions.

## P2 — Track selection and auxiliary content

- [ ] **Rich track selection.** Select by language, role, codecs, and accessibility;
  allow user preferences and multiple audio tracks. Today selection is MIME-type only.
- [ ] **Subtitles / captions.** `text/vtt`, TTML, and in-band caption tracks.
- [ ] **Thumbnails / trick-play.** Image adaptation sets (`image/jpeg`) and trick-play
  tracks.
- [ ] **EssentialProperty / SupplementalProperty.** Respect descriptors for codec/scheme
  compatibility and role signalling.

## P3 — Observability and extensibility

- [ ] **Metrics API.** Throughput history, buffer level, startup delay, rebuffer events,
  and bitrate-switch history (feeds ABR without influencing playback directly).
- [ ] **Richer event model.** Add `ManifestLoaded`, `BufferUpdated`, `BitrateChanged`,
  `PlaybackStarted`/`Ended`, and error events alongside the current fragment events.
- [ ] **Pluggable HTTP client.** Abstract networking behind a trait so `reqwest` is one
  of several backends (WASM/browser fetch, custom TLS, embedded stacks).
- [ ] **Pluggable ABR.** Introduce an ABR trait/rules engine; keep BOLA as the default
  implementation.

## P4 — Additional DRM and advanced DASH

- [ ] **Additional DRM systems.** ClearKey
- [ ] **Low-Latency DASH.** `availabilityTimeComplete`, `ServiceDescription`, resync
  points, and chunked/partial segment transfer.
- [ ] **In-band and MPD events.** `EventStream`, `emsg`, and SCTE-35 ad markers.
- [ ] **Content steering / MPD updates.** `Location`, content steering, and MPD patch
  (`urn:mpeg:dash:mpd-patch`) updates.
- [ ] **CMCD / CMSD.** Common Media Client/Server Data request and response hints.

## P5 — Conformance and quality

- [ ] **DASH-IF conformance suite.** Expand beyond the single simplified smoke test to a
  broader set of DASH-IF test vectors.
- [ ] **Regression tests for each fix.** Per `AGENTS.md`, every bug fix ships with a
  deterministic regression test.
- [ ] **API surface cleanup.** Resolve the several `#[allow(dead_code)]` public helpers
  (`start_merged`, `into_async_read`, track subscription helpers) once the public API
  stabilises.
