# dashplayrs

A pure Rust implementation of an MPEG-DASH player library.

## Usage

Add to your `Cargo.toml`:

```toml
dashplayrs = "0.1"
```

```rust
use dashplayrs::{Player, PlayerEvent};

#[tokio::main]
async fn main() -> Result<(), dashplayrs::PlayerError> {
    let player = Player::new("https://example.com/manifest.mpd", None)?;
    let outputs = player.start_tracks().await?;

    if let Some(mut rx) = outputs.subscribe(0) {
        while let Ok(event) = rx.recv().await {
            match event {
                PlayerEvent::Init(data) | PlayerEvent::Segment { data, .. } => {
                    // feed `data` to a demuxer / decoder
                }
                PlayerEvent::End => break,
            }
        }
    }

    outputs.join.await.unwrap()?;
    Ok(())
}
```

For DRM-protected streams, pass a Widevine license server URL as the second argument to
[`Player::new`](#player), or supply a custom license fetcher with
[`Player::with_license_fetcher`](#player).

## Public API

### Crate root

| Item | Description |
|------|-------------|
| [`Player`](#player) | High-level playback entry point |
| [`MediaPlayer`](#mediaplayer) | Lower-level DASH coordinator |
| [`PlayerEvent`](#playerevent) | Fragment events on a track stream |
| [`PlayerTrack`](#playertrack) | One adaptation-set broadcast channel |
| [`PlayerOutputs`](#playeroutputs) | Tracks and background task from [`MediaPlayer::start`](#mediaplayer) |
| [`PlayerTrackOutput`](#playertrackoutput) | Per-track handle from [`Player::start_tracks`](#player) |
| [`WidevineLicenseFetcher`](#widevinelicensefetcher) | Custom async Widevine license HTTP handler |
| [`PlayerError`](#playererror) | Unified error type for the playback pipeline |
| [`bola`](#bola) | BOLA adaptive bitrate algorithm |
| [`drm`](#drm) | Widevine license handling and MPD DRM parsing |

---

### `Player`

Convenience wrapper around [`MediaPlayer`](#mediaplayer).

```rust
Player::new(manifest_uri: &str, license_uri: Option<&str>) -> Result<Player, PlayerError>
```

Create a player for the given MPD URL. `license_uri` is an optional fallback Widevine license
server when the manifest does not specify one.

```rust
Player::with_license_fetcher(self, fetcher: WidevineLicenseFetcher) -> Player
```

Replace the default `reqwest` license POST with a custom fetcher (extra headers, cookies, proxy,
etc.).

```rust
Player::start_tracks(self) -> Result<PlayerTrackOutputs, PlayerError>
```

Fetch the manifest, start playback, and return one output handle per audio/video adaptation set.
Each track emits [`PlayerEvent::Init`](#playerevent) followed by
[`PlayerEvent::Segment`](#playerevent) fragments (decrypted when DRM is present).

The returned value also exposes:

- `tracks` — slice of [`PlayerTrackOutput`](#playertrackoutput)
- `join` — await to wait for the background stream controller
- `subscribe(idx)` — subscribe to track `idx` after start
- `into_parts()` — split into tracks, senders, and join handle

```rust
Player::start_merged(self) -> Result<PlayerMergedOutput, PlayerError>
```

Same as `start_tracks`, but merges all track fragments into a single byte stream in arrival
order. Use when you do not need separate audio and video inputs.

---

### `MediaPlayer`

Lower-level DASH client (dash.js `MediaPlayer` equivalent). Prefer [`Player`](#player) unless you
need finer control over manifest loading.

```rust
MediaPlayer::new(uri: &str, license_uri: Option<&str>) -> Result<MediaPlayer, PlayerError>
MediaPlayer::with_license_fetcher(self, fetcher: WidevineLicenseFetcher) -> MediaPlayer
MediaPlayer::fetch_manifest(&mut self) -> Result<(), PlayerError>
MediaPlayer::start(self) -> Result<PlayerOutputs, PlayerError>
```

`start` fetches the manifest, acquires Widevine licenses when needed, and spawns the stream
controller. Subscribe to every [`PlayerTrack`](#playertrack) you care about before relying on
delivery — broadcast channels drop events when there are no receivers.

---

### `PlayerEvent`

Events emitted on a single adaptation-set stream:

| Variant | Payload |
|---------|---------|
| `Init(Bytes)` | Initialization segment (`ftyp` + `moov`) |
| `Segment { number, time, sub_number, data }` | Media segment; `sub_number` is set when `SegmentTimeline/S@k` > 1 |
| `End` | No more fragments for this adaptation set (VOD / bounded window) |

---

### `PlayerTrack`

One DASH adaptation set exposed as a `tokio::sync::broadcast` channel.

| Field / method | Description |
|----------------|-------------|
| `mime_type` | `AdaptationSet@mimeType` when present (e.g. `video/mp4`) |
| `subscribe()` | Create a new event receiver |
| `receiver_count()` | Number of active subscribers |

---

### `PlayerOutputs`

Returned by [`MediaPlayer::start`](#mediaplayer):

| Field | Description |
|-------|-------------|
| `tracks` | One [`PlayerTrack`](#playertrack) per selected audio/video adaptation set |
| `join` | Background task running the stream controller loop |

---

### `PlayerTrackOutput`

Per-track handle returned in `PlayerTrackOutputs.tracks`:

| Field / method | Description |
|----------------|-------------|
| `track_index` | Adaptation-set index |
| `mime_type` | MIME type of the adaptation set |
| `into_receiver()` | Take ownership of the broadcast receiver |
| `events()` | Stream wrapper over track events |

---

### `WidevineLicenseFetcher`

```rust
Arc<dyn Fn(Url, Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Bytes, PlayerError>> + Send>> + Send + Sync>
```

Async callback invoked for Widevine license POSTs instead of the built-in `reqwest` client.

---

### `PlayerError`

Unified error type covering manifest parsing, HTTP, URL resolution, segment fetch failures,
and DRM errors. Notable variants:

| Variant | When |
|---------|------|
| `Manifest` | MPD parse failure |
| `Request` | HTTP client error |
| `Url` | Invalid URL |
| `ManifestNotLoaded` | Operation before manifest fetch |
| `SegmentRequestFailed { status, url }` | Non-success HTTP response for a segment |
| `SegmentBlacklisted` | URL previously failed and was skipped |
| `License` | Widevine license or decryption failure |
| `DrmMpd` | MPD DRM metadata parse failure |

See [`src/lib.rs`](src/lib.rs) for the full list.

---

### `bola`

Buffer Occupancy based Lyapunov Algorithm (BOLA) for adaptive bitrate selection. Used
internally by the player; exposed for custom ABR integrations.

| Type | Description |
|------|-------------|
| `QualityLevel` | One rung on the bitrate ladder (`label`, `bitrate_bps`, `utility`) |
| `Bola` | Stateful ABR decision engine |
| `BolaDecision` | Chosen quality index, estimated segment size, and emergency flag |

| Method | Description |
|--------|-------------|
| `Bola::new(qualities, ewma_alpha)` | Build from a quality ladder |
| `Bola::observe_throughput(bps)` | Update throughput EWMA |
| `Bola::update_buffer(seconds)` | Update buffer occupancy |
| `Bola::decide()` | Select the next representation |
| `Bola::buffer_s()` / `throughput_bps()` / `v()` / `qualities()` | Inspect state |

---

### `drm`

Widevine DRM support.

**Re-exported at `dashplayrs::drm`:**

| Type | Description |
|------|-------------|
| `License` | Widevine session with decrypt capability |
| `LicenseError` | License acquisition or decryption error |
| `WidevineLicenseManager` | Cache of ready license sessions |
| `WidevineSessionKey` | Session identity derived from a PSSH box |

**`dashplayrs::drm::mpd`:**

| Type / function | Description |
|-----------------|-------------|
| `MpdDrmInfo` | Parsed DRM metadata for the full MPD |
| `PeriodDrmInfo` / `AdaptationSetDrmInfo` / `RepresentationDrmInfo` | Per-level DRM with inheritance |
| `LevelDrmInfo` | Effective PSSH boxes, default KIDs, and license URLs |
| `parse_mpd_drm_info(xml)` | Parse DRM elements from raw MPD XML |

**`dashplayrs::drm::decrypt`:**

| Function | Description |
|----------|-------------|
| `get_cdm()` | Load a Widevine CDM from the `DEVICE_PATH` environment variable |
| `create_license_request(pssh)` | Build a license challenge from a PSSH box |

**`License` methods:**

```rust
License::new_from_pssh(pssh) -> Result<License, LicenseError>
License::challenge() -> Result<Vec<u8>, LicenseError>
License::set_license(bytes) -> Result<(), LicenseError>
License::decrypt(ciphertext, init) -> Result<Bytes, LicenseError>
```

## Development

Requires Rust stable.

Build:

```bash
cargo build
```

Test:

```bash
cargo test
```

Format and lint:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```
