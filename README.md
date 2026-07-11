# dashplayrs

A pure Rust implementation of an MPEG-DASH player library.

## Features

- **MPEG-DASH playback** — VOD and live streams with `SegmentTemplate` addressing (number, time, and duration) and `SegmentTimeline` support, including segment sequences (`S@k`)
- **Live streaming** — Dynamic manifests, time-shift buffer windows, periodic manifest refresh, and multi-period transitions with init re-emission
- **Multi-track output** — Separate audio and video adaptation sets, or a single merged byte stream
- **Track selection** — Ordered language, role, codec, and accessibility preferences with per-kind output limits
- **Adaptive bitrate** — BOLA (Buffer Occupancy based Lyapunov Algorithm) with automatic representation switching and init re-emission on quality changes
- **Widevine DRM** — PSSH and license URL parsing from the MPD, license acquisition, and in-pipeline segment decryption
- **Custom license handling** — Pluggable async license fetcher for custom headers, cookies, or proxies
- **Pluggable HTTP client** — [`HttpClient`](#http-client) trait with a default [`ReqwestClient`](#http-client); swap in browser fetch, embedded stacks, or custom TLS
- **Resilient fetching** — BaseURL resolution and failover, representation fallback, and segment URL blacklisting after failures
- **Clock sync** — `UTCTiming` resolution for live edge calculation (HTTP, NTP/SNTP, and related schemes)
- **Modular API** — High-level [`Player`](#player) wrapper or lower-level [`MediaPlayer`](#mediaplayer) for finer control
- **Playback control** — `seek`, `pause`, `resume`, `stop`, and a [`PlaybackState`](#playbackstate) lifecycle machine
- **Async delivery** — Tokio-based fragment delivery via broadcast channels (`Init`, `Segment`, `End` events)
- **Rich event model** — Lifecycle and observability events (`ManifestLoaded`, `BufferUpdated`, `BitrateChanged`, `PlaybackStarted`/`PlaybackEnded`, `Error`) alongside fragment events
- **Metrics API** — Per-track throughput, buffer level, startup delay, rebuffer events, and bitrate-switch history via [`TrackMetrics`](#metrics)

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
                PlayerEvent::BitrateChanged { to_bitrate_bps, .. } => {
                    println!("switched to {to_bitrate_bps} bps");
                }
                ev if ev.is_terminal() => break, // End, PlaybackEnded, or Error
                _ => {} // ManifestLoaded, BufferUpdated, PlaybackStarted
            }
        }
    }

    outputs.join.await.unwrap()?;
    Ok(())
}
```

For DRM-protected streams, pass a Widevine license server URL as the second argument to
[`Player::new`](#player), or supply a custom license fetcher with
[`Player::with_license_fetcher`](#player). For custom manifest, segment, or clock-sync HTTP
handling, use [`Player::with_http_client`](#http-client).

### Track selection

Use ordered fallback preferences and per-kind limits to choose adaptation sets. The default
retains every audio and video track.

```rust
use dashplayrs::{Player, TrackDescriptor, TrackPreference, TrackSelection};

# async fn example() -> Result<(), dashplayrs::PlayerError> {
let selection = TrackSelection::default()
    .with_audio(
        TrackPreference::default()
            .language("en-NZ")
            .language("en")
            .role("main")
            .accessibility(TrackDescriptor::scheme(
                "urn:tva:metadata:cs:AudioPurposeCS:2007",
            ))
            .max_tracks(2),
    )
    .with_video(
        TrackPreference::default()
            .codec("avc1")
            .max_tracks(1),
    );

let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .with_track_selection(selection)
    .start_tracks()
    .await?;
# outputs.stop()?;
# outputs.join.await.unwrap()?;
# Ok(())
# }
```

Selected tracks expose `TrackInfo` metadata including language, roles, codecs, and accessibility
descriptors. Preferences rank candidates; unmatched tracks are fallback candidates. Use
`max_tracks(0)` to disable a media kind.

### Custom HTTP client

By default, playback uses an internal [`ReqwestClient`](#http-client) for manifest fetches,
segment downloads, `UTCTiming` clock sync, and Widevine license POSTs (unless you replace
license handling with [`Player::with_license_fetcher`](#player)).

To configure the underlying `reqwest` client (user agent, proxy, custom TLS, timeouts):

```rust
use dashplayrs::{Player, ReqwestClient, shared};

# async fn example() -> Result<(), dashplayrs::PlayerError> {
let reqwest = reqwest::Client::builder()
    .user_agent("my-app/1.0")
    .build()
    .expect("http client");

let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .with_http_client(shared(ReqwestClient::new(reqwest)))
    .start_tracks()
    .await?;
# outputs.stop()?;
# outputs.join.await.unwrap()?;
# Ok(())
# }
```

For environments without `reqwest` (browser `fetch`, embedded stacks, corporate proxies),
implement [`HttpClient`](#http-client) and pass a shared handle via
[`Player::with_http_client`](#player) or [`MediaPlayer::with_http_client`](#mediaplayer):

```rust
use dashplayrs::{
    HttpClient, HttpError, HttpRequest, HttpResponse, Player, shared,
};
use std::future::Future;
use std::pin::Pin;

struct MyHttpClient;

impl HttpClient for MyHttpClient {
    fn send<'a>(
        &'a self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>> {
        Box::pin(async move {
            // Dispatch `request` to your stack and map the result to `HttpResponse`.
            let _ = request;
            Err(HttpError::Transport("not implemented".into()))
        })
    }
}

# async fn example() -> Result<(), dashplayrs::PlayerError> {
let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .with_http_client(shared(MyHttpClient))
    .start_tracks()
    .await?;
# outputs.stop()?;
# outputs.join.await.unwrap()?;
# Ok(())
# }
```

[`HttpRequest`](#http-client) supports `GET`, `HEAD`, and `POST` with optional headers and
inclusive byte ranges (`byte_range(start, end)`). HTTP failures surface as
[`PlayerError::Request`](#playererror) ([`HttpError`](#http-client)).

### Playback control

After [`Player::start_tracks`](#player) or [`MediaPlayer::start`](#mediaplayer), use
[`PlaybackController`](#playbackcontroller) (available as `outputs.playback`) to manage the
session:

```rust
use std::time::Duration;
use dashplayrs::{PlaybackState, Player};

# async fn example() -> Result<(), dashplayrs::PlayerError> {
let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .start_tracks()
    .await?;

outputs.pause()?;                       // suspend segment delivery
outputs.resume()?;                      // continue from the current position
outputs.seek(Duration::from_secs(30))?; // jump to 30 s into the presentation
outputs.stop()?;                        // halt playback and emit End

assert_eq!(outputs.playback_state(), PlaybackState::Ended);

outputs.join.await.unwrap()?;
# Ok(())
# }
```

[`PlayerTrackOutputs`](#playertrackoutputs) also exposes `pause`, `resume`, `seek`, `stop`,
`playback_state`, and `subscribe_playback_state` as convenience wrappers around the same
controller. Clone handles (`outputs.playback.clone()`) share one session.

## Public API

### Crate root

| Item | Description |
|------|-------------|
| [`Player`](#player) | High-level playback entry point |
| [`MediaPlayer`](#mediaplayer) | Lower-level DASH coordinator |
| [`PlayerEvent`](#playerevent) | Fragment, lifecycle, and observability events on a track stream |
| [`PlayerEventError`](#playerevent) | Error message delivered via [`PlayerEvent::Error`](#playerevent) |
| [`PlayerTrack`](#playertrack) | One adaptation-set broadcast channel |
| [`PlayerOutputs`](#playeroutputs) | Tracks and playback controller from [`MediaPlayer::start`](#mediaplayer) |
| [`PlayerTrackOutput`](#playertrackoutput) | Per-track handle from [`Player::start_tracks`](#player) |
| [`PlayerTrackOutputs`](#playertrackoutputs) | Multi-track session from [`Player::start_tracks`](#player) |
| [`PlayerMergedOutput`](#playermergedoutput) | Merged byte stream from [`Player::start_merged`](#player) |
| [`PlayerMergedAsyncRead`](#playermergedoutput) | `AsyncRead` adapter for piping merged output |
| [`PlaybackController`](#playbackcontroller) | Seek, pause, resume, stop, and lifecycle state |
| [`PlaybackState`](#playbackstate) | Explicit playback lifecycle enum |
| [`PlaybackControlError`](#playbackcontrolerror) | Errors from playback control commands |
| [`TrackMetrics`](#metrics) | Per-track playback metrics collector |
| [`TrackMetricsSnapshot`](#metrics) | Point-in-time metrics view (throughput, buffer, switches, rebuffers) |
| [`ThroughputSample`](#metrics) / [`BufferSample`](#metrics) / [`BitrateSwitch`](#metrics) / [`RebufferEvent`](#metrics) | Individual metric samples |
| `TrackSelection` / `TrackPreference` | Ordered adaptation-set preferences and per-kind limits |
| `TrackInfo` / `TrackKind` | Metadata and media kind for a selected track |
| `TrackDescriptor` | Accessibility descriptor scheme/value matcher and metadata |
| [`WidevineLicenseFetcher`](#widevinelicensefetcher) | Custom async Widevine license HTTP handler |
| [`HttpClient`](#http-client) / [`ReqwestClient`](#http-client) | Pluggable HTTP transport for manifest, segment, and clock-sync requests |
| [`HttpRequest`](#http-client) / [`HttpResponse`](#http-client) / [`HttpError`](#http-client) | Request/response types for custom HTTP backends |
| `shared` | Wrap a concrete [`HttpClient`](#http-client) in [`SharedHttpClient`](#http-client) |
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
Player::with_track_selection(self, selection: TrackSelection) -> Player
Player::with_http_client(self, client: SharedHttpClient) -> Player
```

Replace the default `reqwest` license POST with a custom fetcher (extra headers, cookies, proxy,
etc.). `with_http_client` replaces the default [`ReqwestClient`](#http-client) used for manifest,
segment, and `UTCTiming` requests.

```rust
Player::start_tracks(self) -> Result<PlayerTrackOutputs, PlayerError>
```

Fetch the manifest, start playback, and return one output handle per audio/video adaptation set.
Each track emits [`PlayerEvent::Init`](#playerevent) followed by
[`PlayerEvent::Segment`](#playerevent) fragments (decrypted when DRM is present).

This convenience API spawns one background Tokio task for the stream controller and returns its
[`JoinHandle`](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html) as `join`. For
caller-owned concurrency, use [`MediaPlayer::start`](#mediaplayer) and
[`PlayerOutputs::run`](#playeroutputs) instead.

The returned value also exposes:

- `tracks` — slice of [`PlayerTrackOutput`](#playertrackoutput)
- `playback` — [`PlaybackController`](#playbackcontroller) for seek / pause / resume / stop
- `join` — await to wait for the background stream controller
- `pause` / `resume` / `seek` / `stop` — convenience wrappers around `playback`
- `playback_state` / `subscribe_playback_state` — observe lifecycle transitions
- `subscribe(idx)` — subscribe to track `idx` after start
- `into_parts()` — split into tracks, senders, and join handle

```rust
Player::start_merged(self) -> Result<PlayerMergedOutput, PlayerError>
```

Same as `start_tracks`, but merges all track fragments into a single byte stream in arrival
order. Use when you do not need separate audio and video inputs.

---

### `PlayerMergedOutput`

Returned by [`Player::start_merged`](#player):

| Field / method | Description |
|----------------|-------------|
| `stream` | Merged init + media fragments as a [`ReceiverStream`](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.ReceiverStream.html) |
| `join` | Background task running the stream controller and fragment forwarding |
| `into_async_read()` | Convert to [`PlayerMergedAsyncRead`](#playermergedoutput) for piping into a child process (e.g. `ffmpeg -i pipe:0`) |

---

### `MediaPlayer`

Lower-level DASH client (dash.js `MediaPlayer` equivalent). Prefer [`Player`](#player) unless you
need finer control over manifest loading.

```rust
MediaPlayer::new(uri: &str, license_uri: Option<&str>) -> Result<MediaPlayer, PlayerError>
MediaPlayer::with_license_fetcher(self, fetcher: WidevineLicenseFetcher) -> MediaPlayer
MediaPlayer::with_track_selection(self, selection: TrackSelection) -> MediaPlayer
MediaPlayer::with_http_client(self, client: SharedHttpClient) -> MediaPlayer
MediaPlayer::fetch_manifest(&mut self) -> Result<(), PlayerError>
MediaPlayer::start(self) -> Result<PlayerOutputs, PlayerError>
```

`start` fetches the manifest, acquires Widevine licenses when needed, and returns playback
handles. It does **not** spawn a background task — call [`PlayerOutputs::run`](#playeroutputs)
on the current async task, or [`PlayerOutputs::spawn`](#playeroutputs) for a separate Tokio
task. Subscribe to every [`PlayerTrack`](#playertrack) you care about before relying on
delivery — broadcast channels drop events when there are no receivers.

---

### `PlayerEvent`

Events emitted on a single adaptation-set stream. **Fragment** events carry media bytes;
**lifecycle** and **observability** events report manifest, buffer, bitrate, and playback state.

| Variant | Kind | Payload / meaning |
|---------|------|-------------------|
| `Init(Bytes)` | Fragment | Initialization segment (`ftyp` + `moov`) |
| `Segment { number, time, sub_number, data }` | Fragment | Media segment; `sub_number` is set when `SegmentTimeline/S@k` > 1 |
| `ManifestLoaded { is_dynamic, media_presentation_duration }` | Lifecycle | An MPD was fetched and parsed (initial load or live refresh) |
| `BufferUpdated { buffer_s }` | Observability | Consumer-reported buffer occupancy changed (emitted by [`BufferFeedback::report`](#bufferfeedback)) |
| `BitrateChanged { from_quality_index, to_quality_index, from_bitrate_bps, to_bitrate_bps }` | Observability | The active representation changed on the ladder |
| `PlaybackStarted` | Lifecycle | First media segment delivered for this adaptation set |
| `PlaybackEnded` | Lifecycle | Playback finished (VOD end, stop, or bounded window); precedes `End` |
| `Error(PlayerEventError)` | Lifecycle | Pipeline failed; the full [`PlayerError`](#playererror) is still returned by `join` |
| `End` | Fragment | No more fragments for this adaptation set (VOD / bounded window) |

Helper methods classify events without matching every variant:

| Method | Returns `true` for |
|--------|--------------------|
| `is_terminal()` | `End`, `PlaybackEnded`, `Error` |
| `is_fragment()` | `Init`, `Segment` |

`PlayerEventError` is a clone-friendly wrapper around the error message string (`PlayerEventError(pub String)`),
so track events remain `Clone` even though [`PlayerError`](#playererror) is not.

---

### `PlayerTrack`

One DASH adaptation set exposed as a `tokio::sync::broadcast` channel.

| Field / method | Description |
|----------------|-------------|
| `mime_type` | `AdaptationSet@mimeType` when present (e.g. `video/mp4`) |
| `info` | Selected-track language, roles, codecs, accessibility, ID, and media kind |
| `subscribe()` | Create a new event receiver |
| `receiver_count()` | Number of active subscribers |
| `buffer_feedback()` | Report playback buffer occupancy for ABR |
| `metrics()` | [`TrackMetrics`](#metrics) collector for this track |

---

### `BufferFeedback`

Consumer-reported buffer level (seconds of media buffered ahead of the playhead) used by
the internal BOLA ABR controller.

| Method | Description |
|--------|-------------|
| `report(buffer_s)` | Update the buffer level seen by ABR for this track |

Report periodically as the decoder or renderer consumes media so bitrate decisions reflect
actual playback state.

---

### Metrics

Per-track playback metrics are collected as a side effect of delivery and buffer feedback.
Metrics observe download and buffer behaviour without influencing playback or ABR decisions
directly. Obtain a [`TrackMetrics`](#metrics) handle from `outputs.metrics(idx)`
([`PlayerTrackOutputs`](#playertrackoutputs)), `track.metrics()` ([`PlayerTrack`](#playertrack)),
or `output.metrics()` ([`PlayerTrackOutput`](#playertrackoutput)). Clone handles share one
session.

```rust
use dashplayrs::Player;

# async fn example() -> Result<(), dashplayrs::PlayerError> {
let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .start_tracks()
    .await?;
let metrics = outputs.metrics(0).expect("track 0");

// Point-in-time view:
let snap = metrics.snapshot();
println!("throughput: {} bps, buffer: {} s", snap.throughput_bps, snap.buffer_s);

// Or watch updates as they are recorded:
let mut rx = metrics.subscribe();
while rx.changed().await.is_ok() {
    let snap = rx.borrow();
    if let Some(delay) = snap.startup_delay {
        println!("startup delay: {delay:?}");
    }
}
# Ok(())
# }
```

**`TrackMetrics` methods:**

| Method | Description |
|--------|-------------|
| `snapshot()` | Current [`TrackMetricsSnapshot`](#metrics) |
| `subscribe()` | [`watch::Receiver`](https://docs.rs/tokio/latest/tokio/sync/watch/struct.Receiver.html) of snapshots, seeded with the current value |

**`TrackMetricsSnapshot` fields:**

| Field | Description |
|-------|-------------|
| `startup_delay: Option<Duration>` | Time from stream start to the first delivered segment |
| `buffer_s: f64` | Latest consumer-reported buffer level (seconds) |
| `throughput_bps: f64` | Smoothed (EWMA) download throughput estimate |
| `throughput_history: Vec<ThroughputSample>` | Per-segment throughput samples (`elapsed`, `throughput_bps`, `bytes`, `download_duration`) |
| `buffer_history: Vec<BufferSample>` | Reported buffer occupancy over time (`elapsed`, `buffer_s`) |
| `bitrate_switch_history: Vec<BitrateSwitch>` | Representation switches (`from`/`to` quality index and bitrate) |
| `rebuffer_events: Vec<RebufferEvent>` | Buffer drops below the low-water mark after playback began (`elapsed`, `buffer_s`) |

History series are bounded (most recent samples retained). Each sample's `elapsed` is measured
from the start of metrics collection for that track.

---

### `PlayerTrackOutputs`

Returned by [`Player::start_tracks`](#player):

| Field / method | Description |
|----------------|-------------|
| `tracks` | One [`PlayerTrackOutput`](#playertrackoutput) per adaptation set |
| `playback` | [`PlaybackController`](#playbackcontroller) for this session |
| `join` | Background task running the stream controller loop |
| `pause` / `resume` / `seek` / `stop` | Playback control (delegates to `playback`) |
| `playback_state` / `subscribe_playback_state` | Current or watched [`PlaybackState`](#playbackstate) |
| `buffer_feedback(idx)` | [`BufferFeedback`](#bufferfeedback) for a track index |
| `metrics(idx)` | [`TrackMetrics`](#metrics) for a track index |
| `subscribe(idx)` | Subscribe to a track's broadcast channel |
| `track_count()` | Number of adaptation-set tracks in this session |

---

### `PlayerOutputs`

Returned by [`MediaPlayer::start`](#mediaplayer):

| Field / method | Description |
|----------------|-------------|
| `tracks` | One [`PlayerTrack`](#playertrack) per selected audio/video adaptation set |
| `playback` | [`PlaybackController`](#playbackcontroller) for this session |
| `run()` | Run the stream controller on the current async task (no spawn) |
| `spawn()` | Spawn the stream controller as a separate Tokio task |

---

### `PlaybackController`

Lifecycle controls for an active playback session. Returned as `outputs.playback` from
[`Player::start_tracks`](#player) and [`MediaPlayer::start`](#mediaplayer). Clone handles share
the same session.

| Method | Description |
|--------|-------------|
| `pause()` | Suspend segment delivery until `resume` |
| `resume()` | Resume delivery after `pause` |
| `seek(presentation_time)` | Seek to a presentation time ([`Duration`](https://doc.rust-lang.org/std/time/struct.Duration.html) from the start of the presentation) |
| `stop()` | Stop playback; no further segments are delivered |
| `state()` | Current [`PlaybackState`](#playbackstate) |
| `subscribe_state()` | Watch lifecycle state transitions |

Commands return [`PlaybackControlError`](#playbackcontrolerror) when playback is not active or
has already stopped.

---

### `PlaybackState`

Explicit lifecycle state for the playback pipeline:

| Variant | Meaning |
|---------|---------|
| `Idle` | No active playback session |
| `LoadingManifest` | Fetching or refreshing the MPD |
| `Buffering` | Waiting for media to begin or resume |
| `Playing` | Segments are being delivered |
| `Paused` | Delivery suspended until `resume` |
| `Seeking` | Repositioning to a new presentation time |
| `Ended` | Manifest window exhausted or playback stopped |
| `Error` | Pipeline failed; inspect `join` for details |

---

### `PlaybackControlError`

| Variant | When |
|---------|------|
| `NotActive` | Command issued before playback started |
| `Stopped` | Command issued after `stop` or natural end |

---

### `PlayerTrackOutput`

Per-track handle returned in `PlayerTrackOutputs.tracks`:

| Field / method | Description |
|----------------|-------------|
| `track_index` | Adaptation-set index |
| `mime_type` | MIME type of the adaptation set |
| `info` | Selected-track language, roles, codecs, accessibility, ID, and media kind |
| `into_receiver()` | Take ownership of the broadcast receiver |
| `buffer_feedback()` | Report playback buffer occupancy for ABR |
| `metrics()` | [`TrackMetrics`](#metrics) collector for this track |
| `events()` | Stream wrapper over track events |

---

### `WidevineLicenseFetcher`

```rust
Arc<dyn Fn(Url, Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Bytes, PlayerError>> + Send>> + Send + Sync>
```

Async callback invoked for Widevine license POSTs instead of the built-in HTTP client when
[`Player::with_license_fetcher`](#player) is set.

---

### HTTP client

Networking is abstracted behind [`HttpClient`](src/http/mod.rs) so playback is not tied to a
single HTTP library. The default backend is [`ReqwestClient`](src/http/reqwest.rs).

| Type / function | Description |
|-----------------|-------------|
| `HttpClient` | Async trait: `send(HttpRequest) -> Result<HttpResponse, HttpError>` |
| `SharedHttpClient` | `Arc<dyn HttpClient>` shared across playback tasks |
| `shared(client)` | Wrap a concrete client for use with `with_http_client` |
| `ReqwestClient` | Default backend; `ReqwestClient::new(reqwest::Client)` for custom `reqwest` settings |
| `HttpRequest` | `get` / `head` / `post` builders with `header` and `byte_range` |
| `HttpResponse` | Status, headers, and body; `is_success()`, `header(name)`, `text()`, `into_bytes()` |
| `HttpError` | Transport or body decode failure |

Used for:

- MPD manifest fetches
- Init and media segment downloads (including HTTP `Range` requests)
- `UTCTiming` clock synchronization (`GET`, `HEAD`)
- Default Widevine license POSTs (override with [`WidevineLicenseFetcher`](#widevinelicensefetcher))

Configure on [`Player`](#player) or [`MediaPlayer`](#mediaplayer):

```rust
Player::with_http_client(self, client: SharedHttpClient) -> Player
MediaPlayer::with_http_client(self, client: SharedHttpClient) -> MediaPlayer
```

---

### `PlayerError`

Unified error type covering manifest parsing, HTTP, URL resolution, segment fetch failures,
and DRM errors. Notable variants:

| Variant | When |
|---------|------|
| `Manifest` | MPD parse failure |
| `Request` | HTTP client error ([`HttpError`](#http-client)) |
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
