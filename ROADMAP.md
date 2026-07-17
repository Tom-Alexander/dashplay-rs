# Roadmap

Status: `[ ]` not started · `[~]` partial · `[x]` done.

---

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
- [x] **ABR inputs.** Live latency / playback rate (LoL+); dropped-frame signals via
  host `PlaybackQualityFeedback` (dash.js `DroppedFramesRule` /
  `getVideoPlaybackQuality()`).
  *dash.js:* `droppedFramesRule` uses `getVideoPlaybackQuality()`; LL catch-up feeds
  latency into playback rate.
- [~] **Pause semantics.** Buffer drain signalling; optional in-flight cancel.
  *dash.js:* `scheduleWhilePaused` (default true); `HTTPLoader.abort()` cancels
  in-flight + pending retries.
- [~] **Live DVR seek.** Expand seek bounds beyond the resolved timeline.
  *dash.js:* DVR window vs availability window; `getDvrWindow` / seek across sliding
  live multiperiod.
- [x] **`BaseURL@availabilityTimeOffset`.** Honour BaseURL-level ATO.
  *dash.js:* Uses BaseURL ATO when segment-level ATO is absent; core LL availability.
