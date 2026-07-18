import init, { DashPlayer } from "./pkg/dashplay_wasm.js";

const form = document.getElementById("play-form");
const manifestInput = document.getElementById("manifest-url");
const licenseInput = document.getElementById("license-url");
const deviceInput = document.getElementById("device-wvd");
const playButton = document.getElementById("play-button");
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const errorEl = document.getElementById("error");

/** Keep roughly this many seconds of MSE media ahead of the playhead for live. */
const LIVE_BUFFER_KEEP_S = 25;

/**
 * @typedef {{
 *   mime: string,
 *   sourceBuffer: SourceBuffer,
 *   queue: Uint8Array[],
 *   appending: boolean,
 *   timestampOffsetReady: boolean,
 *   timescale: number,
 * }} TrackSink
 */

/** @type {Map<number, TrackSink>} */
const tracks = new Map();

let wasmReady = false;
let mediaSource = null;
let currentPlayer = null;
let isLiveManifest = false;

function setStatus(message) {
  statusEl.textContent = message;
}

function setError(message) {
  errorEl.textContent = message ?? "";
}

/** Prefer the highest AVC profile/level so ABR upswitches stay SourceBuffer-compatible. */
function pickCodec(codecs) {
  if (!codecs?.length) {
    return null;
  }
  const avc = codecs.filter((c) => /^avc1\./i.test(c));
  if (avc.length > 0) {
    return [...avc].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" })).at(-1);
  }
  return codecs[0];
}

function mimeForTrack(track) {
  const mime = track.mime_type ?? "video/mp4";
  const codec = pickCodec(track.codecs);
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

/**
 * Walk ISO-BMFF boxes and invoke `visit(type, payloadStart, payloadEnd)`.
 * Return a value from `visit` to stop early.
 */
function walkBoxes(bytes, start, end, visit) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = start;
  while (offset + 8 <= end) {
    let size = view.getUint32(offset);
    let header = 8;
    if (size === 1) {
      if (offset + 16 > end) {
        return null;
      }
      const hi = view.getUint32(offset + 8);
      const lo = view.getUint32(offset + 12);
      size = hi * 2 ** 32 + lo;
      header = 16;
    } else if (size === 0) {
      size = end - offset;
    }
    if (size < header || offset + size > end) {
      return null;
    }

    const type = String.fromCharCode(
      bytes[offset + 4],
      bytes[offset + 5],
      bytes[offset + 6],
      bytes[offset + 7],
    );
    const payloadStart = offset + header;
    const payloadEnd = offset + size;

    const early = visit(type, payloadStart, payloadEnd, view);
    if (early !== undefined) {
      return early;
    }

    if (
      type === "moov" ||
      type === "trak" ||
      type === "mdia" ||
      type === "minf" ||
      type === "stbl" ||
      type === "moof" ||
      type === "traf"
    ) {
      const nested = walkBoxes(bytes, payloadStart, payloadEnd, visit);
      if (nested !== null && nested !== undefined) {
        return nested;
      }
    }

    offset += size;
  }
  return null;
}

function readMdhdTimescale(bytes) {
  return walkBoxes(bytes, 0, bytes.length, (type, payloadStart, payloadEnd, view) => {
    if (type !== "mdhd") {
      return undefined;
    }
    const version = bytes[payloadStart];
    if (version === 0 && payloadStart + 20 <= payloadEnd) {
      return view.getUint32(payloadStart + 12);
    }
    if (version === 1 && payloadStart + 28 <= payloadEnd) {
      return view.getUint32(payloadStart + 20);
    }
    return undefined;
  });
}

/**
 * Read `baseMediaDecodeTime` from the first `tfdt` in an fMP4 fragment.
 * @returns {{ decodeTime: number, timescale: number } | null}
 */
function readTfdtTiming(bytes, timescaleHint) {
  const decodeTime = walkBoxes(bytes, 0, bytes.length, (type, payloadStart, payloadEnd, view) => {
    if (type !== "tfdt") {
      return undefined;
    }
    const version = bytes[payloadStart];
    if (version === 0 && payloadStart + 8 <= payloadEnd) {
      return view.getUint32(payloadStart + 4);
    }
    if (version === 1 && payloadStart + 12 <= payloadEnd) {
      const hi = view.getUint32(payloadStart + 4);
      const lo = view.getUint32(payloadStart + 8);
      return hi * 2 ** 32 + lo;
    }
    return undefined;
  });
  if (decodeTime === null || decodeTime === undefined) {
    return null;
  }
  const timescale = timescaleHint && timescaleHint > 0 ? timescaleHint : 1;
  return { decodeTime, timescale };
}

function isInitSegment(bytes) {
  if (bytes.length < 8) {
    return false;
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let offset = 0; offset + 8 <= bytes.length; ) {
    let size = view.getUint32(offset);
    let header = 8;
    if (size === 1) {
      if (offset + 16 > bytes.length) {
        break;
      }
      const hi = view.getUint32(offset + 8);
      const lo = view.getUint32(offset + 12);
      size = hi * 2 ** 32 + lo;
      header = 16;
    } else if (size === 0) {
      size = bytes.length - offset;
    }
    if (size < header) {
      break;
    }
    const type = String.fromCharCode(
      bytes[offset + 4],
      bytes[offset + 5],
      bytes[offset + 6],
      bytes[offset + 7],
    );
    if (type === "moov" || type === "ftyp") {
      return true;
    }
    if (type === "moof" || type === "mdat") {
      return false;
    }
    offset += size;
  }
  return false;
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

  // Use segments mode + timestampOffset for live. Safari/WebKit often fatal-errors
  // (`PIPELINE_ERROR_DECODE` / VT -12909) when sequence mode + ABR init switches reorder
  // format changes relative to non-keyframes (see WebKit bug 314035).
  const sourceBuffer = source.addSourceBuffer(mime);
  if ("mode" in sourceBuffer) {
    sourceBuffer.mode = "segments";
  }
  const sink = {
    mime,
    sourceBuffer,
    queue: [],
    appending: false,
    timestampOffsetReady: false,
    timescale: 0,
  };

  sourceBuffer.addEventListener("updateend", () => {
    sink.appending = false;
    maybeSeekToBufferedStart();
    pruneLiveBuffer(sink);
    drainQueue(sink);
  });

  sourceBuffer.addEventListener("error", () => {
    setError(`SourceBuffer error for ${track.kind} (${mime})`);
  });

  tracks.set(track.index, sink);
  return sink;
}

function maybeSeekToBufferedStart() {
  if (!video.paused || tracks.size === 0) {
    return;
  }
  try {
    let start = Infinity;
    for (const sink of tracks.values()) {
      const buffered = sink.sourceBuffer.buffered;
      if (buffered.length > 0) {
        start = Math.min(start, buffered.start(0));
      }
    }
    if (Number.isFinite(start) && start > 0.25 && Math.abs(video.currentTime - start) > 0.25) {
      video.currentTime = start;
    }
  } catch {
    // Ignore transient InvalidStateError while buffers settle.
  }
}

function pruneLiveBuffer(sink) {
  if (!isLiveManifest || sink.sourceBuffer.updating) {
    return;
  }
  try {
    const buffered = sink.sourceBuffer.buffered;
    if (buffered.length === 0) {
      return;
    }
    const end = buffered.end(buffered.length - 1);
    const keepFrom = Math.max(buffered.start(0), end - LIVE_BUFFER_KEEP_S);
    if (keepFrom - buffered.start(0) > 1.0) {
      sink.sourceBuffer.remove(buffered.start(0), keepFrom);
    }
  } catch {
    // remove() may throw while appending; next updateend retries.
  }
}

function prepareChunk(sink, chunk) {
  const bytes = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);

  if (isInitSegment(bytes)) {
    const timescale = readMdhdTimescale(bytes);
    if (timescale && timescale > 0) {
      sink.timescale = timescale;
    }
    return bytes;
  }

  if (!sink.timestampOffsetReady) {
    const timing = readTfdtTiming(bytes, sink.timescale);
    if (timing && timing.timescale > 0) {
      const pts = timing.decodeTime / timing.timescale;
      try {
        sink.sourceBuffer.timestampOffset = -pts;
        sink.timestampOffsetReady = true;
        console.log("timestampOffset", sink.mime, -pts);
      } catch (err) {
        console.warn("timestampOffset failed", err);
      }
    } else {
      sink.timestampOffsetReady = true;
    }
  }

  return bytes;
}

function drainQueue(sink) {
  if (sink.appending || sink.queue.length === 0) {
    return;
  }
  if (sink.sourceBuffer.updating) {
    return;
  }

  const chunk = sink.queue.shift();
  const bytes = prepareChunk(sink, chunk);
  sink.appending = true;
  try {
    sink.sourceBuffer.appendBuffer(bytes);
  } catch (err) {
    sink.appending = false;
    setError(`appendBuffer failed: ${err}`);
  }
}

function enqueueFragment(trackIndex, bytes) {
  const sink = tracks.get(trackIndex);
  if (!sink) {
    return;
  }
  sink.queue.push(bytes);
  drainQueue(sink);
}

async function startPlayback(manifestUrl, { licenseUrl, deviceFile } = {}) {
  setError("");
  setStatus("initializing WASM…");
  playButton.disabled = true;
  tracks.clear();
  currentPlayer = null;
  isLiveManifest = false;

  if (!wasmReady) {
    await init();
    wasmReady = true;
  }

  // Reset MediaSource so SourceBuffers from a previous play attempt cannot linger.
  if (mediaSource) {
    try {
      if (video.src) {
        URL.revokeObjectURL(video.src);
      }
    } catch {
      // ignore
    }
    mediaSource = null;
    video.removeAttribute("src");
    video.load();
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
  if (licenseUrl) {
    player.set_license_url(licenseUrl);
  }
  if (deviceFile) {
    setStatus("loading Widevine device…");
    const deviceBytes = new Uint8Array(await deviceFile.arrayBuffer());
    player.set_widevine_device(deviceBytes);
  }

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
        setError(`Failed to start playback: ${e}`);
      });
    }
  });

  player.on_status((message) => {
    if (message === "manifest:live") {
      isLiveManifest = true;
      if (mediaSource?.readyState === "open") {
        try {
          mediaSource.duration = Number.POSITIVE_INFINITY;
        } catch (err) {
          console.warn("Could not set live MediaSource duration", err);
        }
      }
      setStatus("live manifest loaded");
      return;
    }
    if (message === "manifest:vod") {
      isLiveManifest = false;
      setStatus("vod manifest loaded");
      return;
    }
    setStatus(message);
  });

  player.on_error((message) => {
    setError(message);
    playButton.disabled = false;
  });

  video.onerror = () => {
    const detail = video.error?.message ?? "unknown video error";
    setError(`Video element error: ${detail}`);
    playButton.disabled = false;
  };

  currentPlayer = player;
  player.start();
  setStatus("starting playback…");
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  const licenseUrl = licenseInput?.value?.trim() || undefined;
  const deviceFile = deviceInput?.files?.[0] || undefined;
  startPlayback(manifestInput.value.trim(), { licenseUrl, deviceFile }).catch((err) => {
    setError(String(err));
    playButton.disabled = false;
  });
});
