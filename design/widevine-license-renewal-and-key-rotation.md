# Widevine license renewal and key rotation

**Status:** Draft  
**ROADMAP:** P1 — License renewal / key rotation  
**Authors:** —  
**Last updated:** 2026-07-11

---

## Summary

Long-running live DASH sessions protected by Widevine eventually need **new content keys**
(key rotation) and **refreshed licenses** (license renewal). Today `dashplayrs` acquires
Widevine licenses once at `MediaPlayer::start` and never updates them during manifest refresh
or playback. Decryption therefore fails when keys rotate or licenses expire mid-session.

This design extends the existing DRM pipeline so licenses remain valid and decryptors hold
all active content keys for the lifetime of a live session, without changing the public
segment-delivery contract (`PlayerEvent::{Init, Segment, End}`).

---

## Motivation

Live streams commonly:

1. **Rotate encryption keys** on a schedule (e.g. every few minutes) for security.
2. **Expire Widevine licenses** and require renewal before the renewal deadline signaled in
   the license response.

Reference players (dash.js, Shaka, ExoPlayer) handle both by maintaining CDM sessions across
MPD updates, merging new keys into active decryptors, and posting renewal challenges to the
license server before expiry.

`dashplayrs` already:

- Parses Widevine PSSH and license URLs from the MPD (`drm::mpd`).
- Acquires licenses at startup and caches them in `WidevineLicenseManager`.
- Decrypts segments in `dash_stream` using `License::decrypt`.

But the playback loop (`PlaybackLoopState` in `stream_controller.rs`) refreshes the manifest
without re-evaluating DRM state, and passes a **frozen** license snapshot into adaptation
streams. A session that outlives the first license acquisition window will fail at the first
rotated segment or expired license.

---

## Background

### DASH key rotation signals

Key rotation can be signaled through several channels (not mutually exclusive):

| Signal | Where | Typical use |
|--------|-------|-------------|
| MPD `ContentProtection` | Refreshed MPD | New `cenc:pssh` and/or `default_KID` |
| Initialization segment | `moov` / `pssh` / `tenc` | In-band PSSH or KID change at period or rep switch |
| Media segment | `emsg` (`urn:mpeg:dash:mp4:event:2012`) | Key-rotation event with embedded PSSH |
| CMAF | `emsg` in fragment | Same as above for LL-DASH / CMAF live |

During overlap, segments encrypted with the **previous** KID may still be in the buffer window
while new segments use the **next** KID. The decryptor must hold **multiple content keys**
simultaneously.

### Widevine license renewal

A Widevine license response includes:

- **CONTENT** keys — used for decryption.
- **KEY_CONTROL** keys — control blocks (no key material) that encode renewal timing and
  policy. The CDM uses them to decide when to issue a **renewal** license request for the
  same session.

Renewal uses the **same** CDM session as the initial license (same session id in the
challenge/response), unlike key rotation which may open a new session when PSSH changes.

The `widevine` crate (0.1.0) parses `KEY_CONTROL` in `KeySet` but exposes only initial
`get_license_request` + `CdmLicenseRequest::get_keys`. Renewal challenge generation is not
yet part of the public API.

---

## Current implementation

```text
MediaPlayer::start
    │
    ├─ fetch_manifest + parse_mpd_drm_info
    ├─ for each AdaptationSet / Representation:
    │     WidevineLicenseManager::get(PSSH)  ── hit → reuse
    │     else License::new_from_pssh → challenge → POST → set_license
    └─ spawn PlaybackLoopState { adaptation_wv_sessions, ... }

PlaybackLoopState::run  (loop)
    │
    ├─ fetch_manifest          ← MPD only; DRM not re-parsed
    ├─ spawn run_adaptation_stream(license snapshot)
    └─ sleep(minimumUpdatePeriod)
```

Relevant types:

| Type | Role | Limitation |
|------|------|------------|
| `WidevineSessionKey` | Hash of full PSSH bytes | Correct for dedup; unused on refresh |
| `WidevineLicenseManager` | `HashMap<SessionKey, Arc<License>>` | Never consulted after `start` |
| `License` | Holds `CdmLicenseRequest` + `Ap4CencDecryptingProcessor` | `set_license` **replaces** processor; no merge |
| `PlaybackLoopState` | Manifest refresh + stream orchestration | No license manager, no DRM parse, no HTTP license fetch |

Decrypt path (`dash_stream.rs`):

- Picks `wv_by_rep[rep_id]` or adaptation-set fallback `license`.
- Calls `lic.decrypt(data, init)`; errors propagate unless fragment is clear.

---

## Goals

1. **Manifest-driven key rotation:** After each live MPD refresh, detect new or changed
   effective PSSH / `default_KID` at Period, AdaptationSet, and Representation levels and
   acquire missing licenses before segments need them.
2. **Multi-key decryptors:** A single logical session accumulates content keys across
   rotations so in-window old segments still decrypt.
3. **License renewal:** Track renewal deadlines from `KEY_CONTROL` and proactively renew
   before expiry during long sessions.
4. **Live-session reliability:** Failures to acquire a required key surface as
   `PlayerError::License` with actionable context (KID, PSSH hash, license URL).
5. **Minimal API churn:** Keep `Player::new(manifest, license_uri)` and segment events
   unchanged for the first phase; optional richer DRM events deferred to P3.

## Non-goals (this design)

- PlayReady, FairPlay, or ClearKey (ROADMAP P4).
- Full `emsg` / EventStream / SCTE-35 pipeline (ROADMAP P4).
- Browser CDM integration (library remains device-file + external CDM oriented).
- Automatic license server authentication beyond existing `WidevineLicenseFetcher`.
- Changing ABR or segment-delivery semantics.

In-band PSSH from init segments and `emsg` are **phase 2** scope (see rollout); phase 1
covers MPD refresh rotation, which is the most common live pattern.

---

## Requirements

### Functional

| ID | Requirement |
|----|-------------|
| F1 | On manifest refresh, re-parse DRM info and diff against active sessions. |
| F2 | Acquire licenses for PSSH values not present in `WidevineLicenseManager`. |
| F3 | Merge new CONTENT keys into the active decryptor without dropping prior keys. |
| F4 | Update adaptation-stream license handles when new sessions become available. |
| F5 | Schedule license renewal from `KEY_CONTROL` before server-indicated expiry. |
| F6 | Support representation-specific sessions when rep-level PSSH differs from AS-level. |
| F7 | Re-use existing sessions when refreshed MPD carries unchanged PSSH (no redundant POST). |

### Non-functional

| ID | Requirement |
|----|-------------|
| NF1 | No panics in library code; errors via `LicenseError` / `PlayerError`. |
| NF2 | License POST remains async I/O; no hidden background tasks beyond the existing playback loop. |
| NF3 | Thread-safe sharing of `Arc<License>` across adaptation streams (unchanged model). |
| NF4 | Deterministic unit tests without a real CDM where possible (PSSH diff, key merge). |

---

## Proposed architecture

### Component changes

```text
┌─────────────────────────────────────────────────────────────────┐
│                     PlaybackLoopState                           │
│  ┌──────────────┐  ┌────────────────────┐  ┌─────────────────┐ │
│  │ manifest     │  │ DrmSessionCoordinator │  │ renewal scheduler│ │
│  │ refresh      │─▶│ (new)                │◀─│ (new)           │ │
│  └──────────────┘  └──────────┬───────────┘  └─────────────────┘ │
│                               │                                   │
│                    WidevineLicenseManager                         │
│                               │                                   │
│                    Arc<License> per AS / rep                      │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                    run_adaptation_stream (per track)
                                │
                         License::decrypt
```

Introduce **`DrmSessionCoordinator`** (name tentative; module `drm::coordinator` or method
block on `PlaybackLoopState` initially):

- Owns `WidevineLicenseManager`, fallback `license_uri`, and optional `WidevineLicenseFetcher`.
- **`sync_from_mpd(mpd_xml, period_idx)`** — parse DRM, compute required sessions, fetch
  missing licenses, return updated `adaptation_wv_sessions` / `by_rep` vectors.
- **`poll_renewals()`** — called each loop iteration; posts renewal challenges for sessions
  nearing expiry.

Move license acquisition logic out of `MediaPlayer::start` into the coordinator so startup
and refresh share one code path.

### `License` key accumulation

Replace one-shot processor construction with incremental key registration:

```rust
impl License {
    /// Apply a license response; merge CONTENT keys into the decryptor.
    pub fn apply_license(&mut self, license_message: &[u8]) -> Result<(), LicenseError>;

    /// Keys currently loaded (for tests / diagnostics).
    pub fn loaded_kids(&self) -> &[ [u8; 16] ];
}
```

Implementation sketch:

1. Parse keys via existing `CdmLicenseRequest::get_keys`.
2. For each new CONTENT key whose KID is not yet loaded, call
   `Ap4CencDecryptingProcessor::key(kid, key)` on a builder seeded from the existing
   processor, or rebuild from the union of all known keys (acceptable for small key counts).
3. Store `KEY_CONTROL` entries in a `RenewalState` struct attached to the session.

`set_license` becomes a thin alias for `apply_license` for backward compatibility within the
crate.

### Session identity

Keep **`WidevineSessionKey = hash(PSSH bytes)`** for session lookup. When MPD rotation changes
PSSH, a new key maps to a new session; the coordinator may also map KID → session for
decrypt fallback (see open questions).

For renewal, the **same** `License` / `CdmLicenseRequest` instance must be reused; renewal
updates keys in place rather than creating a new `WidevineSessionKey`.

### Wiring into the playback loop

```rust
// stream_controller.rs — each iteration, before spawning streams
let mpd_xml = ...; // retain raw XML on fetch (see below)
self.drm.sync_from_mpd(&mpd_xml, current_window.idx).await?;
self.drm.poll_renewals().await?;

// Pass fresh Arc<License> handles into AdaptationStreamContext
```

**Manifest XML retention:** `PlaybackLoopState::fetch_manifest` currently stores only
`dash_mpd::MPD`. DRM parsing uses raw XML (`parse_mpd_drm_info`). Extend fetch to retain
`mpd_xml: String` alongside the parsed struct (mirroring `MediaPlayer`).

**Mid-iteration session updates:** Adaptation streams run until the current manifest
snapshot is exhausted. For phase 1, refreshing licenses **before** spawning streams each loop
iteration is sufficient: new keys are available at the next refresh boundary. If a stream
task runs long enough to cross a rotation within one iteration, phase 2 adds shared
`Arc<License>` mutation or a watch channel for hot-swapping the decryptor.

Recommended phase 1 approach: store `Arc<License>` in the manager; `apply_license` mutates
through `Arc<Mutex<License>>` **only if** hot-swap is required — prefer **`Arc<RwLock<License>>`**
or an inner `Arc<Ap4CencDecryptingProcessor>` swapped atomically to avoid holding locks
during decrypt. Simpler interim: **`Arc<License>` where decrypt takes a read lock**; license
updates take a write lock briefly when merging keys (decrypt is hot path — measure).

**Preferred pattern (idiomatic Rust):**

```rust
pub struct License {
    inner: Arc<RwLock<LicenseInner>>,
}

struct LicenseInner {
    request: CdmLicenseRequest,
    processor: Ap4CencDecryptingProcessor,
    renewal: RenewalState,
}
```

Decrypt: `self.inner.read()?.processor.decrypt(...)`.  
Apply: `self.inner.write()?.merge_keys(...)`.

This allows the same `Arc<License>` handle to pick up new keys while streams hold a clone.

### Renewal scheduling

```rust
struct RenewalState {
    /// Wall-clock instant before which renewal must complete.
    renew_after: Option<Instant>,
    /// Last renewal attempt (backoff on failure).
    last_attempt: Option<Instant>,
}

impl RenewalState {
    fn update_from_key_control(&mut self, key: &Key) { /* parse control block */ }
    fn needs_renewal(&self, now: Instant) -> bool { /* ... */ }
}
```

Each loop iteration (or a dedicated check before segment download):

1. For each active session, if `needs_renewal`, build renewal challenge.
2. POST via `WidevineLicenseFetcher` or default `reqwest` client.
3. `apply_license` with response.

**Dependency:** extend the `widevine` crate (upstream PR or local fork) to expose renewal
request generation from an existing `CdmLicenseRequest` / session. Until then, document the
gap and implement rotation (F1–F4) first; renewal (F5) lands behind feature flag or follow-up
PR.

---

## Data flows

### MPD refresh key rotation

```text
MPD refresh
    │
    ▼
parse_mpd_drm_info(xml)
    │
    ▼
for each AS / Rep effective PSSH:
    │
    ├─ manager.get(key) ── Some ──▶ ensure mapped in adaptation_wv_sessions
    │
    └─ None ──▶ new License::new_from_pssh
                  │
                  ▼
              POST license server
                  │
                  ▼
              apply_license (merge keys)
                  │
                  ▼
              manager.insert_ready
                  │
                  ▼
              update session vectors
```

### Segment decrypt (unchanged contract)

```text
segment bytes + init bytes
    │
    ▼
rep_license = wv_by_rep[rep_id] or as_license
    │
    ▼
rep_license.decrypt(data, init)  ──▶ PlayerEvent::Segment
```

If decrypt fails with “unknown KID” after rotation, phase 2 may trigger opportunistic
in-band PSSH extraction from `init` and emergency license fetch.

---

## Module layout

| Module | Change |
|--------|--------|
| `drm::widevine` | `apply_license`, `RwLock` inner, `RenewalState`, `loaded_kids` |
| `drm::coordinator` (new) | `DrmSessionCoordinator`, shared acquire/sync/renew logic |
| `media_player.rs` | Delegate startup DRM setup to coordinator; pass coordinator into loop state |
| `stream_controller.rs` | Retain MPD XML; call `sync_from_mpd` / `poll_renewals` each iteration |
| `dash_stream.rs` | No change phase 1 if `Arc<License>` hot-updates work |

Public API: **unchanged** for phase 1. Optional later:

```rust
// P3 — not in initial implementation
PlayerEvent::DrmSessionUpdated { adaptation_index, kids: Vec<[u8;16]> }
PlayerEvent::DrmRenewalFailed { ... }
```

---

## Rollout phases

### Phase 1 — MPD refresh rotation (MVP)

- [ ] Retain raw MPD XML in `PlaybackLoopState`.
- [ ] Extract `DrmSessionCoordinator` from `MediaPlayer::start`.
- [ ] `sync_from_mpd` on each live loop iteration.
- [ ] `License::apply_license` with key merging; `Arc<RwLock<...>>` for live updates.
- [ ] Integration test with two MPD fixtures (PSSH v1 → PSSH v2) served sequentially.

**Exit criteria:** Live loop with rotating PSSH in MPD decrypts segments from both key epochs.

### Phase 2 — In-band rotation

- [ ] Parse PSSH / default KID from fetched init segments (`moov/trak/mdia/minf/stbl/stsd/...`).
- [ ] Optional: handle `emsg` key-rotation events in media segments.
- [ ] On decrypt KID mismatch, trigger coordinator acquire for discovered PSSH.

### Phase 3 — License renewal

- [ ] Parse `KEY_CONTROL` renewal timing from license responses.
- [ ] Upstream or patched `widevine` renewal challenge API.
- [ ] `poll_renewals` with exponential backoff on failure.
- [ ] Long-session test (simulated expiry via mock license server).

---

## Testing strategy

### Unit tests

| Test | Validates |
|------|-----------|
| `apply_license` merges second KID without removing first | F3 |
| `sync_from_mpd` skips POST when PSSH unchanged | F7 |
| `sync_from_mpd` acquires new session when PSSH changes | F1, F2 |
| `RenewalState::needs_renewal` from synthetic KEY_CONTROL | F5 (phase 3) |

### Fixtures

Add under `tests/fixtures/`:

| Fixture | Purpose |
|---------|---------|
| `drm_widevine_rotate/` | Two MPD variants differing in `cenc:pssh` / `default_KID` |
| `drm_widevine_rotate/init_*.mp4` | Init segments matching each key epoch (clear or synthetic) |
| Mock license server (test helper) | Returns canned license bytes for PSSH A and B |

Reuse `common::FixtureServer` pattern; optionally add `?mpd=2` path or time-based switch
after N requests.

### Integration tests

1. **`live_key_rotation_mpd_refresh`** — dynamic MPD, `minimumUpdatePeriod`, server returns
   second MPD with new PSSH; assert segments decrypt (requires `DEVICE_PATH` gate like
   existing `drm.rs` tests).
2. **`license_manager_reuses_session`** — unit-level with mocked license bytes.

Every bug fix in this area requires a regression test per `AGENTS.md`.

---

## Error handling

| Condition | Behaviour |
|-----------|-----------|
| New PSSH but no license URL | Log + `PlayerError::License` when encrypted segment arrives |
| License POST 4xx/5xx | Retry with backoff (max N); fail stream with context |
| Renew failure before expiry | Retry; on expiry decrypt fails with explicit renewal error |
| Clear segment with DRM session | Existing passthrough (keep current behaviour) |
| PSSH parse error on refresh | `PlayerError::DrmMpd`; do not silently drop DRM |

---

## Risks and open questions

| Item | Notes |
|------|-------|
| **`widevine` renewal API** | Phase 3 blocked until crate exposes renewal challenges. Evaluate PR to `widevine` crate vs. internal proto usage. |
| **Processor rebuild cost** | Rebuilding `Ap4CencDecryptingProcessor` on every merge is O(keys); acceptable if ≤10 keys. Benchmark if needed. |
| **Lock contention** | `RwLock` on decrypt path; renewal/merge is rare. Prefer read-heavy pattern. |
| **Session ↔ KID mapping** | When multiple sessions exist for one adaptation set (rotation), should `wv_by_rep` point to latest or a session set? Proposal: single `Arc<License>` per logical stream that accumulates all keys; rep-specific only when rep PSSH differs **and** keys are not merged across reps. |
| **Hidden tasks** | Coordinator runs inside existing playback loop task — aligns with ROADMAP item on explicit lifecycle. |
| **Multi-period live** | Period transition may change ContentProtection; coordinator must run per period index (already tracked via `last_period_idx`). |

---

## References

- [ROADMAP.md](../ROADMAP.md) — P1 license renewal / key rotation
- [ARCHITECTURE.md](../ARCHITECTURE.md) — pipeline stages, dash.js mapping
- [MPEG-DASH Content Protection](https://dashif.org/guidelines/) — DASH-IF IOP encryption
- [CMAF CBCS / key rotation](https://dashif.org/Guidelines-TimingModel/) — timing and rotation practice
- [Widevine DRM Architecture](https://developers.google.com/widevine/drm/overview) — license renewal overview
- Existing code: `src/drm/widevine.rs`, `src/drm/mpd.rs`, `src/media_player.rs`, `src/stream_controller.rs`, `src/dash_stream.rs`

---

## Appendix: dash.js correspondence

| dash.js | dashplayrs (proposed) |
|---------|------------------------|
| `ProtectionController` | `DrmSessionCoordinator` |
| `LicenseRequest` / `LicenseResponse` | `License::apply_license` |
| `KeySystem` session map | `WidevineLicenseManager` |
| `ManifestUpdater` DRM hook | `sync_from_mpd` on refresh |
| `LicenseRenewal` timer | `RenewalState` + `poll_renewals` |

As with the rest of the codebase, behaviour aligns with dash.js concepts but uses explicit
Rust ownership rather than event-bus indirection.
