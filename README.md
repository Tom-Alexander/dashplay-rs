# dashplay-rs

A pure Rust MPEG-DASH player library: manifest parsing, timeline resolution, ABR,
segment scheduling, HTTP download, and optional Widevine decryption.

## Features

- VOD and live DASH (including low-latency / CMAF)
- Multi-track output (audio, video, text, trick-play, thumbnails)
- Pluggable ABR (BOLA by default; LoL+ for low-latency live)
- Pluggable HTTP (`reqwest` native; `fetch` on wasm)
- Widevine DRM (`cenc` / `cens` / `cbc1` / `cbcs`) behind the `drm` feature
- Seek, pause, resume, mid-playback track switching, quality constraints
- Timed events (MPD `EventStream`, in-band `emsg`, SCTE-35 helpers)

### Cargo features

| Feature | Default | Purpose |
|---------|---------|---------|
| `drm` | yes | Widevine license acquisition and segment decryption |
| `reqwest-http` | yes | Native HTTP via `reqwest` |
| `full-runtime` | yes | Tokio multi-thread runtime extras used by examples/tests |

## Usage

```toml
dashplay = "0.1"
```

```rust
use dashplay::{Player, PlayerEvent};

#[tokio::main]
async fn main() -> Result<(), dashplay::PlayerError> {
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
                ev if ev.is_terminal() => break,
                _ => {}
            }
        }
    }

    outputs.join.await.unwrap()?;
    Ok(())
}
```

`Player` is the high-level entry point (spawns the stream controller). Prefer
`MediaPlayer` when you want to own the async task via `PlayerOutputs::run` /
`spawn`.

### Track selection

Audio and video are enabled by default; text, trick-play, and image tracks are
not. Prefer languages, roles, codecs, and caps with `TrackSelection`:

```rust
use dashplay::{Player, TrackPreference, TrackSelection};

async fn example() -> Result<(), dashplay::PlayerError> {
  let selection = TrackSelection::default()
    .with_audio(TrackPreference::default().language("en").max_tracks(2))
    .with_video(TrackPreference::default().codec("avc1").max_tracks(1))
    .with_text(TrackPreference::default().language("en").max_tracks(1));
  
  let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .with_track_selection(selection)
    .start_tracks()
    .await?;

  outputs.stop()?;
  Ok(())
}
```

Text and image payloads are delivered as `Init` / `Segment` bytes — the library
does not parse or render captions or thumbnail tiles.

### Playback control

```rust
use std::time::Duration;
use dashplay::Player;

async fn example() -> Result<(), dashplay::PlayerError> {
  let outputs = Player::new("https://example.com/manifest.mpd", None)?
      .start_tracks()
      .await?;

  outputs.pause()?;
  outputs.resume()?;
  outputs.seek(Duration::from_secs(30))?;
  outputs.stop()?;
  outputs.join.await.unwrap()?;
  Ok(())
}
```

Clone `outputs.playback` to share one session across tasks.

### DRM

Pass a Widevine license server URL to `Player::new`, or supply a custom fetcher
with `Player::with_license_fetcher`. Requires the `drm` feature and a Widevine
device (native: `DEVICE_PATH`; wasm: `set_widevine_device_bytes`).

### HTTP and ABR

```rust
use dashplay::{
    LolPlusAbrFactory, Player, QualityConstraints, ReqwestClient, shared,
    shared_abr_factory,
};

async fn example() -> Result<(), dashplay::PlayerError> {
  let reqwest = reqwest::Client::builder()
    .user_agent("my-app/1.0")
    .build()
    .expect("http client");

let outputs = Player::new("https://example.com/manifest.mpd", None)?
    .with_http_client(shared(ReqwestClient::new(reqwest)))
    .with_abr_factory(shared_abr_factory(LolPlusAbrFactory::default()))
    .with_quality_constraints(
        QualityConstraints::default()
            .min_bitrate_bps(300_000)
            .max_bitrate_bps(2_000_000),
    )
    .start_tracks()
    .await?;
  outputs.stop()?;
  Ok(())
}
```

Implement `HttpClient` or `AbrFactory` for fully custom stacks. On `wasm32`
without `reqwest-http`, `FetchClient` is the default HTTP backend.

### Merged output

`Player::start_merged` concatenates all track fragments into one byte stream
(useful for piping into `ffmpeg`). Prefer `start_tracks` when tracks must stay
separate.

## Examples

```bash
cargo run --example write_stream -- <manifest-url> <output.mp4>
cargo run --example play_gstreamer --features example-gstreamer -- <manifest-url>
```

## Development

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all
```

## License

MIT
