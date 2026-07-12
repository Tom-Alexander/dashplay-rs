# Roadmap

This roadmap tracks the gap between the current implementation and the full player
described in [`ARCHITECTURE.md`](ARCHITECTURE.md).

`dashplayrs` today is a **DASH segment-delivery pipeline**: it parses MPDs, resolves
timelines, applies BOLA ABR, fetches (and optionally Widevine-decrypts) fragmented MP4
init/media segments, and delivers them as `PlayerEvent::{Init, Segment, End}` over
broadcast channels. It is not yet a complete player framework.

Priorities follow the project ordering: **correctness → standards compliance →
reliability → maintainability → performance → API ergonomics**.

Status legend: `[ ]` not started · `[~]` partial · `[x]` done · `[—]` out of scope.

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
| Demux / decode | `[—]` | Out of scope (bytes only) |
| Metrics / rich events | `[~]` | Per-track [`TrackMetrics`]; fragment + [`MediaEvent`] events |
| Pluggable networking / ABR | `[x]` | HTTP client trait + `ReqwestClient`; ABR trait + `BolaAbrFactory` |
| MPD model / remote documents | `[ ]` | xlink, Preselection, metadata elements |
| Buffer-target scheduling | `[ ]` | Downloads not throttled by buffer or `minBufferTime` |
| Bitstream / AS switching | `[ ]` | Init always re-emitted; no cross-AS switch |
| Containers beyond fMP4/CMAF | `[ ]` | mp2t, WebM, additional image MIME types |

---

## P0 — Correctness and standards foundations

These close the largest gaps between "delivers some streams" and "handles conformant DASH".

- [x] **SegmentTemplate inheritance flattening.** Resolve `SegmentTemplate` declared at
  `Period` and `MPD` level, not just on the `AdaptationSet`.
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

- [ ] **Additional DRM systems.** ClearKey, PlayReady, FairPlay, and other
  `ContentProtection` schemes beyond Widevine.
- [~] **Low-Latency DASH.** `availabilityTimeComplete`, `ServiceDescription`, resync
  points, and chunked/partial segment transfer are done; remaining: `Resync@type` 0/1
  recovery, non-`Latency` `ServiceDescription` elements, player-side target-latency control.
- [x] **In-band producer reference time.** Parse `prft` boxes for
  `ProducerReferenceTime@inband=true` clock correction.
- [x] **Mid-segment resync.** Use `Resync@type` 2/3 random-access points during seek
  and playback recovery.
- [x] **Producer-reference integration coverage.** Test live-window selection when
  `ProducerReferenceTime` intentionally diverges from `UTCTiming`.
- [~] **In-band and MPD events.** `EventStream`, `emsg`, and SCTE-35 ad markers are done;
  remaining: AdaptationSet/Representation MPD `EventStream`, SCTE-35 splice decode, other
  binary `Event@messageData` schemes.
- [~] **Content steering / MPD updates.** `Location`, content steering, and MPD patch
  (`urn:mpeg:dash:mpd-patch`) updates are done; remaining: `PatchLocation@ttl`, multiple
  patch locations, patch failure surfacing, conditional GET, DCSM TTL-driven reload,
  steering beyond BaseURL reorder.
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

## P5b — Code structure and maintainability

Structural refactors that align the codebase with [`ARCHITECTURE.md`](ARCHITECTURE.md)
without changing playback behaviour.

- [x] **`manifest/` module split.** Decompose monolithic `manifest.rs` into focused
  submodules (`timeline/`, `addressing`, `template`, `sidx/`, etc.).
- [x] **`schedule/` module split.** Replace `dash_stream.rs` with `adaptation_stream.rs`
  and fetch orchestration.
- [x] **`track_selection/` module split.** Separate kind, selection, and descriptor logic.
- [x] **`abr/bola/` consolidation.** Keep BOLA algorithm and factory under `abr/bola/`.
- [x] **`schedule/fetch.rs` decomposition.** Split into `segment_emit.rs` (events +
  metrics), `segment_fetch.rs` (HTTP + rep fallback + sidx), and `segment_decrypt.rs`
  (media-fragment decryption).
- [ ] **`PlayerError` domain split.** Replace the monolithic `PlayerError` enum in
  `lib.rs` with per-subsystem error types (`ManifestError`, `SegmentError`, etc.) and a
  top-level wrapper.
- [ ] **`stream_controller` extraction.** Pull manifest-refresh loop, period-context
  building, and MPD event dedup out of `PlaybackLoopState::run`; type manifest session
  state so `expect("parsed")` is unnecessary.
- [x] **Root module clustering.** Group loose `src/` modules into subtrees: `mp4/`
  (`mp4_box`, `prft`, `partial_segment`, in-band `emsg` parsing), manifest lifecycle
  (`manifest_update`, `content_steering`, `mpd_patch`), clock/live-edge (`utc_timing`,
  `resync`).
- [x] **`descriptors` → `track_selection/`.** Move adaptation-set compatibility
  filtering next to its only consumers.
- [ ] **`manifest/tests.rs` distribution.** Split the ~900-line catch-all test module
  into per-submodule `tests.rs` files (mirroring `timeline/tests.rs`).
- [ ] **DRM renewal split.** Separate `renewal.rs` CDM protocol parsing (`kctl`, license
  policy) from session scheduling (`RenewalState`, backoff, poll timing).
- [ ] **Scheduler / fetch separation.** Introduce a synchronous `SegmentPlan` type
  (segment index, rep, init needed, byte range) produced by scheduling logic and consumed
  by fetch/decrypt/emit — prerequisite for buffer-target scheduling (P7).
- [ ] **`TrackSessionState` consolidation.** Replace the many `Arc<Mutex<…>>` handles
  passed through `AdaptationStreamContext` with a single per-track session struct.
- [ ] **`manifest/mod.rs` targeted re-exports.** Replace `pub(crate) use …::*` barrel
  exports with explicit re-exports or submodule paths so module boundaries stay visible.
- [x] **Stale doc references.** Update remaining docs that still reference removed files
  (`dash_stream.rs`, monolithic `manifest.rs`).

---

## P6 — Segment addressing and MPD model (unsupported backlog)

- [x] **Remaining template variables.** `$Width$`, `$Height$`, `$FrameRate$`, `$Ext$`,
  `$Initialization$`.
- [x] **`SegmentTemplate@endNumber`.** Bound static `@duration` segment counts without
  relying solely on Period/MPD duration.
- [x] **`SegmentTemplate@index` (sidecar index).** Inherited in the model; fetch and use
  separate index documents.
- [x] **Per-segment `SegmentTemplate@index` URLs.** Support `$Number$` / `$Time$`
  substitution in index templates that point to one index document per media segment.
- [ ] **`RepresentationIndex` addressing.** Fetch and parse `RepresentationIndex`
  child elements as an alternative to `@index` / `@indexRange`.
- [ ] **`SegmentTemplate@maxDuration`.** Validate or bound segment durations.
- [ ] **Bitstream switching.** Honour `SegmentTemplate@bitstreamSwitching` /
  `BitstreamSwitching` — skip init fetch/re-emit when switching representations.
- [~] **`SegmentList` byte ranges.** `SegmentURL@mediaRange` HTTP Range fetch; byte-range-only
  list addressing without timeline.
- [ ] **Hierarchical `sidx`.** `reference_type ≠ 0` index references.
- [~] **Whole-file `SegmentBase`.** Single-segment and `@presentationDuration` progressive
  paths without `@indexRange`.
- [~] **`@indexRangeExact`.** Distinct semantics from `@indexRange`.
- [ ] **Sidecar `@indexRangeExact`.** Apply exact-range semantics to
  `SegmentTemplate@index` sidecar index fetches.
- [ ] **Addressing-mode validation.** Enforce mutual exclusivity of `SegmentTemplate` /
  `SegmentList` / `SegmentBase` at the same hierarchy level.
- [ ] **Remote MPD documents.** `MPD@xlink:href`, `Period@xlink:href`, and
  `urn:mpeg:dash:resolve-to-zero:2013` period placeholders.
- [ ] **`Preselection`.** Preselected adaptation-set bundles.
- [ ] **`SubRepresentation`.** Sub-track selection and template inheritance.
- [~] **`EssentialProperty` breadth.** Accept common codec/compatibility schemes instead of
  excluding adaptation sets with unknown essential properties.
- [~] **`SupplementalProperty` playback semantics.** Execute adaptation-set switching and
  other supplemental signalling beyond metadata collection.
- [ ] **`AdaptationSet/Switching` and `RandomAccess`.** Seamless AS switch and explicit
  random-access hints beyond SAP-aligned seek.
- [ ] **MPD metadata elements.** `ProgramInformation`, `Metrics` (DASH reporting
  descriptors), `AssetIdentifier`, `Rating`, `Period/Label`, `Representation/Label`.
- [~] **`MPD@minBufferTime` and `@maxSegmentDuration`.** Use for startup delay, buffer
  targets, scheduling validation.
- [~] **Profile-specific playback.** `mp2t-main`, `mp2t-simple`, DVB, HbbTV, AC-4, MHA1,
  VP9, VP9-HDR paths beyond conformance validation.
- [~] **AdaptationSet range attributes.** Enforce `@minBandwidth` / `@maxBandwidth` /
  `@minWidth` / `@maxWidth` / `@minHeight` / `@maxHeight` / `@minFrameRate` /
  `@maxFrameRate` against representations and ABR.
- [~] **Period `EventStream` scope.** Collect AdaptationSet- and Representation-level MPD
  `EventStream` events, not only Period-level.
- [~] **Period gaps.** Surface explicit gap / discontinuity signalling between periods.

## P7 — Scheduling, ABR, and playback semantics (unsupported backlog)

- [ ] **Buffer-target scheduling.** Throttle downloads when consumer buffer is full;
  honour `MPD@minBufferTime` for startup and rebuffer recovery.
- [ ] **Manifest-derived BOLA parameters.** Derive segment duration and buffer limits from
  MPD segment durations instead of hardcoded 4 s / 25 s assumptions.
- [ ] **Parallel segment prefetch.** Concurrent segment downloads per track.
- [ ] **ABR inputs.** Playback rate and dropped-frame signals per `ARCHITECTURE.md`.
- [ ] **User quality constraints.** Max/min bitrate cap, fixed quality rung, data-saver mode.
- [ ] **Mid-playback track switching.** Change audio language or subtitles without
  restarting (`TrackSelection` is fixed at `MediaPlayer::start`).
- [ ] **Runtime adaptation-set switching.** `urn:mpeg:dash:adaptation-set-switching:2016`
  seamless cross-AS switch.
- [ ] **Playback rate / fast-forward.** `@maxPlayoutRate` and `@codingDependency` on main
  video.
- [ ] **Automatic stall detection.** Detect rebuffer without requiring consumer
  `BufferFeedback::report`.
- [~] **Pause semantics.** Buffer drain signalling; optional in-flight download cancellation.
- [x] **Playhead API.** Track and expose current presentation time.
- [~] **Live DVR seek.** Expand seek bounds and window handling beyond resolved timeline.
- [ ] **Dynamic MPD static-duration semantics.** `@type="dynamic"` with static presentation
  duration behaviour.
- [ ] **Multi-period overlap / sync buffer.** Handling beyond init re-emission.
- [~] **LL-DASH target latency control.** Adjust consumption rate to chase
  `ServiceDescription/Latency@target`.

## P8 — Networking, platform, and containers (unsupported backlog)

- [ ] **HTTP retry with backoff.** Transient manifest/segment failures (failover only today).
- [ ] **Conditional manifest fetch.** `If-None-Match` / `304 Not Modified` on live refresh.
- [ ] **Shipped WASM / browser `HttpClient`.** Reference fetch backend for WASM targets.
- [~] **`BaseURL@availabilityTimeOffset`.** Use BaseURL-level ATO, not only segment-level.
- [ ] **DVB and other namespace BaseURL extensions.** e.g. `@dvb:priority` beyond deserialize.
- [ ] **Steering beyond BaseURL reorder.** DCSM features past `SERVICE-LOCATION-PRIORITY`.
- [ ] **MPEG-2 Transport Stream.** `mp2t-main` and `mp2t-simple` profile playback.
- [ ] **WebM / Matroska.** `video/webm`, `audio/webm` segment delivery.
- [ ] **Additional image MIME types.** `image/png`, `image/bmp`, and other thumbnail schemes.
- [~] **Progressive MP4.** Non-fragmented whole-file playback via `SegmentBase`.
- [ ] **Multiplexed A+V.** Single adaptation set carrying multiplexed audio and video.
- [ ] **`UTCTiming` WebSocket scheme.** `urn:mpeg:dash:utc:websocket`.
- [ ] **Other `UTCTiming` schemes.** Any scheme not explicitly handled today.
- [~] **CBCS / pattern encryption.** Document and test non-CTR CENC modes.
- [ ] **Hardware security level and HDCP policy.** Output-protection enforcement.

## P9 — Out of scope (not planned)

These MPEG-DASH-adjacent capabilities are intentionally excluded. Consumers or
higher-level frameworks should provide them.

- [—] **Media demultiplexing** beyond `emsg`, `prft`, PSSH, and LL-DASH chunk boundaries.
- [—] **Sample extraction** — access-unit or timestamped sample output.
- [—] **Decoding** — video, audio, subtitle, or image decode.
- [—] **Rendering / presentation** — UI, A/V sync, compositing.
- [—] **SCTE-35 splice command parsing** — cue bytes are exposed; splice semantics are not decoded.
- [—] **Non-DASH protocols** — HLS, MSS, RTSP, etc.
- [—] **Cookie / credential policy** — callers attach headers via [`HttpRequest`](src/http/request.rs).
