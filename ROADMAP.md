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
- [~] **Pause semantics.** Buffer drain signalling; optional in-flight cancel.
  *dash.js:* `scheduleWhilePaused` (default true); `HTTPLoader.abort()` cancels
  in-flight + pending retries.
- [~] **Live DVR seek.** DVR window API + seek expands duration-template timelines and
  selects periods for backward multi-period rewind; explicit `SegmentTimeline` already
  spans full TSBD.
  *dash.js:* DVR window vs availability window; `getDvrWindow` / seek across sliding
  live multiperiod.
