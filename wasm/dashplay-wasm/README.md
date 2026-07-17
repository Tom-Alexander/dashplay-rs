# dashplay-wasm

Browser proof-of-concept for [`dashplayrs`](../../): compiles the DASH segment pipeline to
WebAssembly, fetches fMP4 fragments with the browser `fetch` API, optionally decrypts
Widevine/CENC in-pipeline (CDM + Bento4 `mp4decrypt`), and renders clear media with
[Media Source Extensions](https://developer.mozilla.org/en-US/docs/Web/API/Media_Source_Extensions_API).

## Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target (use the rustup
  toolchain, not Homebrew `rustc`, so the wasm stdlib is available)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/)
- [wasi-sdk](https://github.com/WebAssembly/wasi-sdk/releases) for compiling Bento4 C++
  via the forked [`bento4-rs`](https://github.com/Tom-Alexander/bento4-rs) (`WASI_SDK_PATH`)
- LLVM `llvm-ar` on `PATH` (e.g. `brew install llvm`)

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
# Example: extract wasi-sdk and export WASI_SDK_PATH=/path/to/wasi-sdk-24.0-arm64-macos
```

## Build

```bash
cd wasm/dashplay-wasm
export WASI_SDK_PATH=/path/to/wasi-sdk   # required for drm / Bento4
export PATH="$(brew --prefix llvm)/bin:$HOME/.cargo/bin:$PATH"
AR=llvm-ar wasm-pack build --target web --out-dir dist/pkg
```

## Run locally

Serve `dist/` over HTTP (required for `fetch` and WASM loading):

```bash
cd wasm/dashplay-wasm/dist
python3 -m http.server 8080
```

Open http://localhost:8080.

- **Clear streams:** play any CORS-enabled DASH MPD (default is Akamai BBB).
- **Encrypted streams:** choose a Widevine `.wvd` device file, optionally set a license
  URL override, then play an encrypted MPD. Segments are decrypted in WASM and appended
  to MSE as clear fMP4 (no browser EME / `MediaKeys`).

## Architecture

```text
  Browser                         WASM (dashplay-wasm)
  -------                         --------------------
  .wvd file ────────────────────► set_widevine_device_bytes
  fetch ◄──────────────────────── dashplayrs::FetchClient (+ license POST)
  Bento4 mp4decrypt ───────────── CENC decrypt (wasi-sdk build)
  MSE SourceBuffer ◄─ on_fragment ─ clear init + media fMP4
  <video>            ◄────────────── decrypted bytes only
```

- **`FetchClient`** — library [`dashplayrs::FetchClient`](../../src/http/fetch.rs) (default on
  `wasm32` without `reqwest-http`) uses `window.fetch`.
- **`DashPlayer`** wraps [`MediaPlayer::start`](../../src/media_player.rs) + [`PlayerOutputs::run`](../../src/types.rs).
- **`dist/app.js`** maps each audio/video track to a `SourceBuffer` and loads an optional
  `.wvd` before start. WASI preview1 stubs live in `dist/wasi_snapshot_preview1.js`,
  resolved via an import map in `index.html` (required by current wasm-bindgen output).

## Limitations (POC)

- CORS: the manifest / license host must allow browser cross-origin requests.
- Text/trick-play/image tracks are ignored.
- Buffer feedback uses a simple segment-count estimate rather than real MSE buffer depth.
- Live streams may need additional MSE lifecycle handling not covered here.
- Browser EME (`MediaKeys`) is out of scope; see ROADMAP optional host path.

## Default test stream

```
https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd
```
