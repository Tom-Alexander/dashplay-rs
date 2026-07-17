# Roadmap

Remaining work toward the player described in [`ARCHITECTURE.md`](ARCHITECTURE.md).

`dashplayrs` is a DASH segment-delivery pipeline: MPD parse, timelines, ABR, fetch,
optional Widevine decrypt, and `PlayerEvent` delivery. Demux, decode, and rendering
are intentionally out of scope.

Priorities: **correctness → standards → reliability → maintainability → performance → API**.

Status: `[ ]` not started · `[~]` partial.

---

## Correctness & standards

- [~] **Content steering / MPD updates.** `Location`, steering reorder, and MPD patch
  done; remaining: `PatchLocation@ttl`, multiple patch locations, patch failure
  surfacing, conditional GET.
  *dash.js:* MPD patch with `PatchLocation` (+ ttl), multi-location select, invalid-patch
  → full refresh; content steering with DCSM TTL / `PATHWAY-PRIORITY` / `RELOAD-URI`.
  No dedicated player-side conditional GET (browser cache may 304).
- [~] **CMCD / CMSD.** Header CMCD + CMSD parse/expose done for v1 keys; remaining:
  query-string `CMCD=`; wire `nor`/`nrr`; extra keys; CMCD on license / UTCTiming /
  xlink; drive ABR/scheduling from CMSD hints.
  *dash.js:* Query mode default + header mode; `nor`/`nrr` and broad v1 key set;
  `includeInRequests` (segment/mpd); CMSD parse with optional ABR cap via `cmsd.abr.applyMb`
  (`mb` / `etp`).
- [~] **AdaptationSet range attributes.** Enforce `@minBandwidth` / `@maxBandwidth` /
  `@minWidth` / `@maxWidth` / `@minHeight` / `@maxHeight` / `@minFrameRate` /
  `@maxFrameRate` against representations and ABR.
  *dash.js:* AS min/max attrs are modelled/metadata; ABR caps come from settings
  (`abr.minBitrate` / `maxBitrate`) and capability filters, not hard enforcement of AS
  range attrs against each Representation.

## Scheduling & ABR

- [~] **ABR inputs.** Live latency / playback rate done (LoL+); dropped-frame signals
  still pending.
  *dash.js:* `droppedFramesRule` uses `getVideoPlaybackQuality()`; LL catch-up feeds
  latency into playback rate.
- [~] **Pause semantics.** Buffer drain signalling; optional in-flight cancel.
  *dash.js:* `scheduleWhilePaused` (default true); `HTTPLoader.abort()` cancels
  in-flight + pending retries.
- [~] **Live DVR seek.** Expand seek bounds beyond the resolved timeline.
  *dash.js:* DVR window vs availability window; `getDvrWindow` / seek across sliding
  live multiperiod.

## Networking & platform

- [~] **`BaseURL@availabilityTimeOffset`.** Honour BaseURL-level ATO.
  *dash.js:* Uses BaseURL ATO when segment-level ATO is absent; core LL availability.
- [x] **DVB / namespace BaseURL extensions.** e.g. `@dvb:priority` beyond deserialize.
  *dash.js:* Parses/uses `dvbPriority` / `dvbWeight` for BaseURL selection.
- [x] **Steering beyond BaseURL reorder.** DCSM features past `SERVICE-LOCATION-PRIORITY`:
  TTL-gated reload, `PATHWAY-PRIORITY`, optional `RELOAD-URI`, `_DASH_pathway` /
  `_DASH_throughput` query params.
  *dash.js:* Full DCSM client: TTL reload, `PATHWAY-PRIORITY`, optional `RELOAD-URI`,
  pathway query params.
- [~] **WASM test player.** MSE demo exists; remaining: real buffer feedback, live MSE
  lifecycle, non-A/V tracks, broader fixtures.
- [x] **WASM DRM (CDM + `mp4decrypt`).** Same in-pipeline Widevine path as native: CDM
  device → license challenge/response → content keys → Bento4 CENC decrypt → clear
  segments to the host. Forked [`bento4-rs`](https://github.com/Tom-Alexander/bento4-rs)
  cross-compiles for `wasm32` via wasi-sdk (`WASI_SDK_PATH`); device bytes via
  `set_widevine_device_bytes` / demo file picker. Optional EME host path still open.

  **Done:**
  1. **`mp4decrypt` / Bento4 for `wasm32`.** Fork of `bento4-rs` with wasm stubs outside
     the Bento4 submodule + wasi-sdk C++ build; shared `Ap4CencDecryptingProcessor` API.
  2. **CDM on WASM.** Device injector (`set_widevine_device_bytes`); DRM timing uses
     `platform::{Instant, utc_now}`; `getrandom`/`js` for challenges.
  3. **`dashplay-wasm`.** `drm` enabled; demo loads `.wvd`, optional license URL,
     appends decrypted fMP4 to MSE; minimal WASI fd stubs via import map.

  **Remaining:**
  4. **Optional EME host path.** Separate later option for apps that must use the browser
     CDM (`MediaKeys`) instead of a provided device: pass-through encrypted segments + DRM
     signalling events. Not a substitute for the CDM + `mp4decrypt` goal.
