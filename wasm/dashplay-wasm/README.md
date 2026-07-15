# dashplay-wasm

Browser proof-of-concept for [`dashplayrs`](../../): compiles the DASH segment pipeline to
WebAssembly, fetches clear fMP4 fragments with the browser `fetch` API, and renders them with
[Media Source Extensions](https://developer.mozilla.org/en-US/docs/Web/API/Media_Source_Extensions_API).

DRM is intentionally excluded. Only clear streams work with this build.

## Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/)

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
```

## Build

```bash
cd wasm/dashplay-wasm
wasm-pack build --target web --out-dir dist/pkg
```

## Run locally

Serve `dist/` over HTTP (required for `fetch` and WASM loading):

```bash
cd wasm/dashplay-wasm/dist
python3 -m http.server 8080
```

Open http://localhost:8080 and play a clear DASH manifest. The default URL is the Akamai
Big Buck Bunny test stream.

## Architecture

```text
  Browser                         WASM (dashplay-wasm)
  -------                         --------------------
  fetch ◄──────────────────────── dashplayrs::FetchClient
  MSE SourceBuffer ◄─ on_fragment ─ MediaPlayer segment events
  <video>            ◄────────────── init + media fMP4 bytes
```

- **`FetchClient`** — library [`dashplayrs::FetchClient`](../../src/http/fetch.rs) (default on
  `wasm32` without `reqwest-http`) uses `window.fetch`.
- **`DashPlayer`** wraps [`MediaPlayer::start`](../../src/media_player.rs) + [`PlayerOutputs::run`](../../src/types.rs) on the browser async runtime (no extra Tokio tasks).
- **`dist/app.js`** maps each audio/video track to a `SourceBuffer` and appends init/segment fragments.

## Limitations (POC)

- Clear streams only (`dashplayrs` built with `default-features = false`, no DRM).
- CORS: the manifest host must allow browser cross-origin requests.
- Text/trick-play/image tracks are ignored.
- Buffer feedback uses a simple segment-count estimate rather than real MSE buffer depth.
- Live streams may need additional MSE lifecycle handling not covered here.

## Default test stream

```
https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd
```
