import init, { DashPlayer } from "./pkg/dashplay_wasm.js";

const form = document.getElementById("play-form");
const manifestInput = document.getElementById("manifest-url");
const playButton = document.getElementById("play-button");
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const errorEl = document.getElementById("error");

/** @type {Map<number, { mime: string, sourceBuffer: SourceBuffer, queue: Uint8Array[], appending: boolean }>} */
const tracks = new Map();

let wasmReady = false;
let mediaSource = null;
let currentPlayer = null;

function setStatus(message) {
  statusEl.textContent = message;
}

function setError(message) {
  errorEl.textContent = message ?? "";
}

function mimeForTrack(track) {
  const mime = track.mime_type ?? "video/mp4";
  const codec = track.codecs?.[0];
  if (!codec) {
    return mime;
  }
  return `${mime}; codecs="${codec}"`;
}

function ensureMediaSource() {
  if (mediaSource) {
    return mediaSource;
  }

  mediaSource = new MediaSource();
  video.src = URL.createObjectURL(mediaSource);
  return mediaSource;
}

function createTrackSink(track) {
  if (track.kind === "text" || track.kind === "image" || track.kind === "trickplay") {
    return null;
  }

  const mime = mimeForTrack(track);
  const source = ensureMediaSource();

  if (source.readyState !== "open") {
    throw new Error("MediaSource is not open yet");
  }

  if (!MediaSource.isTypeSupported(mime)) {
    throw new Error(`Browser does not support ${mime}`);
  }

  const sourceBuffer = source.addSourceBuffer(mime);
  const sink = {
    mime,
    sourceBuffer,
    queue: [],
    appending: false,
  };

  sourceBuffer.addEventListener("updateend", () => {
    sink.appending = false;
    drainQueue(sink);
  });

  tracks.set(track.index, sink);
  return sink;
}

function drainQueue(sink) {
  if (sink.appending || sink.queue.length === 0) {
    return;
  }
  if (sink.sourceBuffer.updating) {
    return;
  }

  const chunk = sink.queue.shift();
  sink.appending = true;
  sink.sourceBuffer.appendBuffer(chunk);
}

function enqueueFragment(trackIndex, bytes) {
  const sink = tracks.get(trackIndex);
  if (!sink) {
    return;
  }
  sink.queue.push(bytes);
  drainQueue(sink);
}

async function startPlayback(manifestUrl) {
  setError("");
  setStatus("initializing WASM…");
  playButton.disabled = true;
  tracks.clear();
  currentPlayer = null;

  if (!wasmReady) {
    await init();
    wasmReady = true;
  }

  const source = ensureMediaSource();

  await new Promise((resolve, reject) => {
    const onOpen = () => {
      source.removeEventListener("sourceopen", onOpen);
      resolve();
    };
    if (source.readyState === "open") {
      resolve();
      return;
    }
    source.addEventListener("sourceopen", onOpen);
    source.addEventListener(
      "error",
      () => reject(new Error("MediaSource failed to open")),
      { once: true },
    );
  });

  const player = new DashPlayer(manifestUrl);

  player.on_track((track) => {

    console.log("track", track);

    try {
      createTrackSink(track);
      setStatus(`track ${track.index}: ${track.kind} (${track.mime_type ?? "unknown"})`);
    } catch (err) {
      setError(String(err));
    }
  });

  player.on_fragment((trackIndex, kind, bytes) => {

    console.log("fragment", trackIndex, kind, bytes.length);

    enqueueFragment(trackIndex, bytes);
    if (kind === "segment" && video.paused) {
      video.play().catch((e) => {
        console.error("Failed to play video", e);
      });
    }
  });

  player.on_status((message) => {
    setStatus(message);
  });

  player.on_error((message) => {
    setError(message);
    playButton.disabled = false;
  });

  currentPlayer = player;
  player.start();
  setStatus("starting playback…");
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  startPlayback(manifestInput.value.trim()).catch((err) => {
    setError(String(err));
    playButton.disabled = false;
  });
});
