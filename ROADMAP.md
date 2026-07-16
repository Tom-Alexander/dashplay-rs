# Roadmap

Remaining work toward the player described in [`ARCHITECTURE.md`](ARCHITECTURE.md).

`dashplayrs` is a DASH segment-delivery pipeline: MPD parse, timelines, ABR, fetch,
optional Widevine decrypt, and `PlayerEvent` delivery. Demux, decode, and rendering
are intentionally out of scope.

Priorities: **correctness → standards → reliability → maintainability → performance → API**.

Status: `[ ]` not started · `[~]` partial.

---

## Correctness & standards

- [~] **In-band / MPD events.** Period-level `EventStream` / `emsg` / SCTE-35 cues done;
  remaining: AdaptationSet/Representation MPD `EventStream`, SCTE-35 splice decode, other
  binary `Event@messageData` schemes.
  *dash.js:* Period `EventStream` + in-band `emsg` only (AS/Rep expose
  `InbandEventStream`, not MPD `EventStream`). SCTE-35 bytes are dispatched to the app;
  no splice-command decode.
- [~] **Content steering / MPD updates.** `Location`, steering reorder, and MPD patch
  done; remaining: `PatchLocation@ttl`, multiple patch locations, patch failure
  surfacing, conditional GET, DCSM TTL reload, steering beyond BaseURL reorder.
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
- [x] **CBCS / pattern encryption.** MPD `mp4protection` `@value` and in-band `schm`
  parsed; decrypt covers `cenc`/`cens`/`cbc1`/`cbcs` via Bento4 (documented + fixtures).
  *dash.js:* Recognises `cenc`/`cbcs` in `ContentProtection`; decryption via EME/CDM.

## Scheduling & ABR

- [x] **Manifest-derived BOLA parameters.** Segment duration and buffer limits from MPD
  instead of hardcoded 4 s / 25 s.
  *dash.js:* `bolaRule` uses measured buffer / fragment timing rather than fixed 4 s /
  25 s constants.
- [ ] **User quality constraints.** Max/min bitrate, fixed quality, data-saver mode.
  *dash.js:* `abr.minBitrate` / `maxBitrate`, `autoSwitchBitrate`,
  `limitBitrateByPortal`; fixed rung via `setQualityFor` / disable autoswitch.
- [ ] **Playback rate / fast-forward.** `@maxPlayoutRate` and `@codingDependency`.
  *dash.js:* `setPlaybackRate` + LL catch-up rate limits; `@maxPlayoutRate` on the
  Representation model. `@codingDependency` is not a driver for trick-play.
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

- [x] **HTTP retry with backoff.** Transient failures (failover only today).
  *dash.js:* Fixed per-type delay (`retryIntervals`), not exponential backoff; default
  ~3 attempts (`retryAttempts`), scaled in low-latency mode.
- [~] **`BaseURL@availabilityTimeOffset`.** Honour BaseURL-level ATO.
  *dash.js:* Uses BaseURL ATO when segment-level ATO is absent; core LL availability.
- [ ] **DVB / namespace BaseURL extensions.** e.g. `@dvb:priority` beyond deserialize.
  *dash.js:* Parses/uses `dvbPriority` / `dvbWeight` for BaseURL selection.
- [ ] **Steering beyond BaseURL reorder.** DCSM features past `SERVICE-LOCATION-PRIORITY`.
  *dash.js:* Full DCSM client: TTL reload, `PATHWAY-PRIORITY`, optional `RELOAD-URI`,
  pathway query params.
- [~] **WASM test player.** MSE demo exists; remaining: real buffer feedback, live MSE
  lifecycle, non-A/V tracks, broader fixtures.

