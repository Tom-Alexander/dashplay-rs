import init, { DashPlayer } from "../../pkg/dashplay_wasm.js";
import { MseSession } from "./session";
import { SubtitleSession } from "./subtitle-session";
import type {
  PlaybackHandle,
  PlaybackOptions,
  PlayerCallbacks,
  TrackInfo,
} from "../types";

let wasmReady = false;

async function ensureWasm(): Promise<void> {
  if (wasmReady) {
    return;
  }
  await init();
  wasmReady = true;
}

/** Seek into buffered media once so the browser paints a frame while paused. */
function showFirstFrame(video: HTMLVideoElement): void {
  const paint = (): boolean => {
    try {
      if (video.buffered.length === 0) {
        return false;
      }
      const start = video.buffered.start(0);
      const target = start + 0.001;
      if (Math.abs(video.currentTime - target) > 0.0005) {
        video.currentTime = target;
      }
      return true;
    } catch {
      return false;
    }
  };

  if (paint()) {
    return;
  }

  const onReady = () => {
    if (paint()) {
      video.removeEventListener("loadeddata", onReady);
      video.removeEventListener("canplay", onReady);
      video.removeEventListener("progress", onReady);
    }
  };

  video.addEventListener("loadeddata", onReady);
  video.addEventListener("canplay", onReady);
  video.addEventListener("progress", onReady);
}

export async function startPlayback(
  video: HTMLVideoElement,
  manifestUrl: string,
  options: PlaybackOptions,
  callbacks: PlayerCallbacks,
): Promise<PlaybackHandle> {
  const { onStatus, onError, onSubtitleTracks } = callbacks;
  const autoplay = options.autoplay !== false;
  let firstFrameRequested = false;
  onError("");
  onStatus("initializing WASM…");
  onSubtitleTracks?.([]);

  await ensureWasm();

  const session = new MseSession(video, onError);
  const subtitles = new SubtitleSession(
    video,
    (tracks) => onSubtitleTracks?.(tracks),
    onStatus,
  );
  session.reset();
  subtitles.reset();
  await session.open();

  const player = new DashPlayer(manifestUrl);
  if (options.licenseUrl) {
    player.set_license_url(options.licenseUrl);
  }
  if (options.deviceFile) {
    onStatus("loading Widevine device…");
    const deviceBytes = new Uint8Array(await options.deviceFile.arrayBuffer());
    player.set_widevine_device(deviceBytes);
  }

  player.on_track((track: TrackInfo) => {
    console.log("track", track);
    try {
      if (track.kind === "text") {
        subtitles.createTrack(track);
        onStatus(
          `track ${track.index}: text (${track.subtitle_type ?? track.mime_type ?? "unknown"})`,
        );
        return;
      }
      session.createTrackSink(track);
      onStatus(`track ${track.index}: ${track.kind} (${track.mime_type ?? "unknown"})`);
    } catch (err) {
      onError(String(err));
    }
  });

  player.on_fragment(
    (
      trackIndex: number,
      kind: string,
      bytes: Uint8Array,
      presentationTimeS?: number,
    ) => {
      console.log("fragment", trackIndex, kind, bytes.length, presentationTimeS);
      if (subtitles.enqueueFragment(trackIndex, kind, bytes, presentationTimeS)) {
        return;
      }
      session.enqueueFragment(trackIndex, bytes);
      if (kind !== "segment" || !video.paused) {
        return;
      }
      if (autoplay) {
        void video.play().catch((e: unknown) => {
          onError(`Failed to start playback: ${String(e)}`);
        });
        return;
      }
      if (!firstFrameRequested) {
        firstFrameRequested = true;
        showFirstFrame(video);
      }
    },
  );

  player.on_status((message: string) => {
    if (message === "manifest:live") {
      session.setLive(true);
      onStatus("live manifest loaded");
      return;
    }
    if (message === "manifest:vod") {
      session.setLive(false);
      onStatus("vod manifest loaded");
      return;
    }
    onStatus(message);
  });

  player.on_error((message: string) => {
    onError(message);
  });

  video.onerror = () => {
    const detail = video.error?.message ?? "unknown video error";
    onError(`Video element error: ${detail}`);
  };

  player.start();
  onStatus("starting playback…");

  return {
    stop: () => {
      subtitles.reset();
      session.reset();
      try {
        player.free();
      } catch {
        // already freed
      }
    },
    setSubtitleTrack: (trackIndex) => {
      subtitles.setActiveTrack(trackIndex);
    },
  };
}
