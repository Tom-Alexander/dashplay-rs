import { isInitSegment, readMdhdTimescale, readTfdtTiming } from "./boxes";
import type { TrackInfo, TrackSink } from "../types";

/** Keep roughly this many seconds of MSE media ahead of the playhead for live. */
const LIVE_BUFFER_KEEP_S = 25;

/** Prefer the highest AVC profile/level so ABR upswitches stay SourceBuffer-compatible. */
function pickCodec(codecs: string[] | undefined): string | null {
  if (!codecs?.length) {
    return null;
  }
  const avc = codecs.filter((c) => /^avc1\./i.test(c));
  if (avc.length > 0) {
    return [...avc].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" })).at(-1) ?? null;
  }
  return codecs[0] ?? null;
}

function mimeForTrack(track: TrackInfo): string {
  const mime = track.mime_type ?? "video/mp4";
  const codec = pickCodec(track.codecs);
  if (!codec) {
    return mime;
  }
  return `${mime}; codecs="${codec}"`;
}

export class MseSession {
  private tracks = new Map<number, TrackSink>();
  private mediaSource: MediaSource | null = null;
  private isLiveManifest = false;
  private objectUrl: string | null = null;
  private readonly video: HTMLVideoElement;
  private readonly onError: (message: string) => void;

  constructor(video: HTMLVideoElement, onError: (message: string) => void) {
    this.video = video;
    this.onError = onError;
  }

  reset(): void {
    this.tracks.clear();
    this.isLiveManifest = false;

    if (this.objectUrl) {
      try {
        URL.revokeObjectURL(this.objectUrl);
      } catch {
        // ignore
      }
      this.objectUrl = null;
    }

    this.mediaSource = null;
    this.video.removeAttribute("src");
    this.video.load();
  }

  setLive(isLive: boolean): void {
    this.isLiveManifest = isLive;
    if (isLive && this.mediaSource?.readyState === "open") {
      try {
        this.mediaSource.duration = Number.POSITIVE_INFINITY;
      } catch (err) {
        console.warn("Could not set live MediaSource duration", err);
      }
    }
  }

  async open(): Promise<MediaSource> {
    const source = this.ensureMediaSource();

    await new Promise<void>((resolve, reject) => {
      if (source.readyState === "open") {
        resolve();
        return;
      }
      const onOpen = () => {
        source.removeEventListener("sourceopen", onOpen);
        resolve();
      };
      source.addEventListener("sourceopen", onOpen);
      source.addEventListener(
        "error",
        () => reject(new Error("MediaSource failed to open")),
        { once: true },
      );
    });

    return source;
  }

  createTrackSink(track: TrackInfo): TrackSink | null {
    if (track.kind === "text" || track.kind === "image" || track.kind === "trickplay") {
      return null;
    }

    const mime = mimeForTrack(track);
    const source = this.ensureMediaSource();

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
    const sink: TrackSink = {
      mime,
      sourceBuffer,
      queue: [],
      appending: false,
      timestampOffsetReady: false,
      timescale: 0,
    };

    sourceBuffer.addEventListener("updateend", () => {
      sink.appending = false;
      this.maybeSeekToBufferedStart();
      this.pruneLiveBuffer(sink);
      this.drainQueue(sink);
    });

    sourceBuffer.addEventListener("error", () => {
      this.onError(`SourceBuffer error for ${track.kind} (${mime})`);
    });

    this.tracks.set(track.index, sink);
    return sink;
  }

  enqueueFragment(trackIndex: number, bytes: Uint8Array): void {
    const sink = this.tracks.get(trackIndex);
    if (!sink) {
      return;
    }
    sink.queue.push(bytes);
    this.drainQueue(sink);
  }

  private ensureMediaSource(): MediaSource {
    if (this.mediaSource) {
      return this.mediaSource;
    }

    this.mediaSource = new MediaSource();
    this.objectUrl = URL.createObjectURL(this.mediaSource);
    this.video.src = this.objectUrl;
    return this.mediaSource;
  }

  private maybeSeekToBufferedStart(): void {
    if (!this.video.paused || this.tracks.size === 0) {
      return;
    }
    try {
      let start = Infinity;
      for (const sink of this.tracks.values()) {
        const buffered = sink.sourceBuffer.buffered;
        if (buffered.length > 0) {
          start = Math.min(start, buffered.start(0));
        }
      }
      if (
        Number.isFinite(start) &&
        start > 0.25 &&
        Math.abs(this.video.currentTime - start) > 0.25
      ) {
        this.video.currentTime = start;
      }
    } catch {
      // Ignore transient InvalidStateError while buffers settle.
    }
  }

  private pruneLiveBuffer(sink: TrackSink): void {
    if (!this.isLiveManifest || sink.sourceBuffer.updating) {
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

  private prepareChunk(sink: TrackSink, chunk: Uint8Array): Uint8Array {
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

  private drainQueue(sink: TrackSink): void {
    if (sink.appending || sink.queue.length === 0) {
      return;
    }
    if (sink.sourceBuffer.updating) {
      return;
    }

    const chunk = sink.queue.shift();
    if (!chunk) {
      return;
    }
    const bytes = this.prepareChunk(sink, chunk);
    sink.appending = true;
    try {
      sink.sourceBuffer.appendBuffer(bytes);
    } catch (err) {
      sink.appending = false;
      this.onError(`appendBuffer failed: ${String(err)}`);
    }
  }
}
