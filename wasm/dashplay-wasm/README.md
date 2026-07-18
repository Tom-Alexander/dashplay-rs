# dashplay-wasm

Browser proof-of-concept for [`dashplay`](../../): compiles the DASH segment pipeline to
WebAssembly, fetches fMP4 fragments with the browser `fetch` API, optionally decrypts
Widevine/CENC in-pipeline (CDM + Bento4 `mp4decrypt`), and renders clear media with
[Media Source Extensions](https://developer.mozilla.org/en-US/docs/Web/API/Media_Source_Extensions_API).

The demo UI is a Vite + React + TypeScript app under `web/`, styled with Tailwind CSS.

## Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target (use the rustup
  toolchain, not Homebrew `rustc`, so the wasm stdlib is available)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/)
- [wasi-sdk](https://github.com/WebAssembly/wasi-sdk/releases) for compiling Bento4 C++
  via the forked [`bento4-rs`](https://github.com/Tom-Alexander/bento4-rs) (`WASI_SDK_PATH`)
- LLVM `llvm-ar` on `PATH` (e.g. `brew install llvm`)
- [Node.js](https://nodejs.org/) 20+ (for the Vite demo)

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
# Example: extract wasi-sdk and export WASI_SDK_PATH=/path/to/wasi-sdk-24.0-arm64-macos
```

## Build WASM

```bash
cd wasm/dashplay-wasm
export WASI_SDK_PATH=/path/to/wasi-sdk   # required for drm / Bento4
export PATH="$(brew --prefix llvm)/bin:$HOME/.cargo/bin:$PATH"
AR=llvm-ar wasm-pack build --target web --out-dir web/pkg
```

## Run the demo

```bash
cd wasm/dashplay-wasm/web
npm install
npm run dev
```

Open http://localhost:8080.

- **Clear streams:** play any CORS-enabled DASH MPD (default includes WebVTT captions).
- **Encrypted streams:** choose a Widevine `.wvd` device file, optionally set a license
  URL override, then play an encrypted MPD. Segments are decrypted in WASM and appended
  to MSE as clear fMP4 (no browser EME / `MediaKeys`).
- **Subtitles:** out-of-band WebVTT / TTML and in-band STPP (TTML-in-fMP4) are rendered
  via the HTML5 `TextTrack` API. Unsupported formats (e.g. WVTT) are listed but ignored.

Production build:

```bash
cd wasm/dashplay-wasm/web
npm run build
npm run preview
```

## Deploy to Cloudflare Pages

The demo is a static Vite build. Build the WASM package first (see above), then either
deploy with Wrangler or connect the repo in the Cloudflare dashboard.

### Wrangler (local or CI)

```bash
cd wasm/dashplay-wasm/web
npm install
npx wrangler login          # once
npm run deploy              # build + wrangler pages deploy
```

Preview branch:

```bash
npm run deploy:preview
```

`wrangler.jsonc` targets the Pages project name `dashplay` and publishes `dist/`.

### GitHub Actions

[`.github/workflows/deploy-demo.yml`](../../.github/workflows/deploy-demo.yml) builds
WASM + the web app and deploys with Wrangler on pushes to `main` (and via
`workflow_dispatch`).

Add repository secrets:

| Secret | Purpose |
|--------|---------|
| `CLOUDFLARE_API_TOKEN` | API token with **Cloudflare Pages — Edit** |
| `CLOUDFLARE_ACCOUNT_ID` | Account ID from the Cloudflare dashboard |

Create the Pages project once if it does not exist yet:

```bash
cd wasm/dashplay-wasm/web
npx wrangler pages project create dashplay --production-branch=main
```

### Dashboard (Git integration)

If you prefer Cloudflare’s Git builds instead of the Actions workflow:

1. Workers & Pages → Create → Pages → Connect to Git
2. Root directory: `wasm/dashplay-wasm/web`
3. Build command: `npm run build`
4. Build output directory: `dist`
5. Environment variable: `NODE_VERSION=22`

Cloudflare’s build image does **not** compile the Rust/WASM package. Either commit a
prebuilt `web/pkg/` or use the GitHub Actions workflow above (recommended).

## Architecture

```text
  Browser                         WASM (dashplay-wasm)
  -------                         --------------------
  .wvd file ────────────────────► set_widevine_device_bytes
  fetch ◄──────────────────────── dashplay::FetchClient (+ license POST)
  Bento4 mp4decrypt ───────────── CENC decrypt (wasi-sdk build)
  MSE SourceBuffer ◄─ on_fragment ─ clear init + media fMP4
  <video>            ◄────────────── decrypted bytes only
```

- **`FetchClient`** — library [`dashplay::FetchClient`](../../src/http/fetch.rs) (default on
  `wasm32` without `reqwest-http`) uses `window.fetch`.
- **`DashPlayer`** wraps [`MediaPlayer::start`](../../src/media_player.rs) + [`PlayerOutputs::run`](../../src/types.rs).
- **`web/`** — Vite/React UI that maps audio/video tracks to `SourceBuffer`s, text tracks to
  HTML5 `TextTrack` cues (WebVTT / TTML / STPP), and loads an optional `.wvd` before start.
  WASI preview1 stubs live in `web/src/wasi_snapshot_preview1.ts`, resolved via a Vite alias
  (required by current wasm-bindgen output).

## Limitations (POC)

- CORS: the manifest / license host must allow browser cross-origin requests.
- Trick-play / image tracks are ignored. Binary WVTT and EIA-608 text are not rendered.
- Buffer feedback uses a simple segment-count estimate rather than real MSE buffer depth.
- Live streams may need additional MSE lifecycle handling not covered here.
- Browser EME (`MediaKeys`) is out of scope; see ROADMAP optional host path.

## Default test stream

```
https://dash.akamaized.net/akamai/test/caption_test/ElephantsDream/elephants_dream_480p_heaac5_1_https.mpd
```

Clear video without captions:

```
https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd
```
