import type { SubtitleTrackOption, TrackInfo } from "../types";
import { cuesFromSubtitleBytes, type SubtitleFormat } from "./subtitles";

export type { SubtitleTrackOption };

type TextSink = {
  info: TrackInfo;
  format: SubtitleFormat;
  supported: boolean;
  textTrack: TextTrack;
  cueKeys: Set<string>;
};

function resolveFormat(track: TrackInfo): SubtitleFormat {
  if (track.subtitle_type) {
    return track.subtitle_type;
  }
  const mime = track.mime_type?.toLowerCase() ?? "";
  if (mime.includes("vtt")) {
    return "vtt";
  }
  if (mime.includes("ttml")) {
    return "ttml";
  }
  if (track.codecs.some((codec) => /^stpp/i.test(codec))) {
    return "stpp";
  }
  if (track.codecs.some((codec) => /^wvtt/i.test(codec))) {
    return "wvtt";
  }
  return "unknown";
}

function isSupported(format: SubtitleFormat): boolean {
  return format === "vtt" || format === "ttml" || format === "stpp";
}

function trackLabel(track: TrackInfo, format: SubtitleFormat): string {
  const language = track.language?.trim();
  const role = track.roles?.[0];
  const parts = [
    language || `Track ${track.index}`,
    role,
    format !== "unknown" ? format.toUpperCase() : undefined,
  ].filter(Boolean);
  return parts.join(" · ");
}

function cueKey(start: number, end: number, text: string): string {
  return `${start.toFixed(3)}|${end.toFixed(3)}|${text}`;
}

export class SubtitleSession {
  private sinks = new Map<number, TextSink>();
  private activeIndex: number | null = null;
  private readonly video: HTMLVideoElement;
  private readonly onTracksChanged: (tracks: SubtitleTrackOption[]) => void;
  private readonly onStatus: (message: string) => void;

  constructor(
    video: HTMLVideoElement,
    onTracksChanged: (tracks: SubtitleTrackOption[]) => void,
    onStatus: (message: string) => void,
  ) {
    this.video = video;
    this.onTracksChanged = onTracksChanged;
    this.onStatus = onStatus;
  }

  reset(): void {
    for (const sink of this.sinks.values()) {
      sink.textTrack.mode = "disabled";
      this.clearCues(sink);
    }
    this.sinks.clear();
    this.activeIndex = null;
    this.onTracksChanged([]);
  }

  createTrack(track: TrackInfo): boolean {
    if (track.kind !== "text") {
      return false;
    }

    const format = resolveFormat(track);
    const supported = isSupported(format);
    const kind = track.roles?.includes("caption") ? "captions" : "subtitles";
    const textTrack = this.video.addTextTrack(
      kind,
      trackLabel(track, format),
      track.language ?? undefined,
    );
    textTrack.mode = "disabled";

    this.sinks.set(track.index, {
      info: track,
      format,
      supported,
      textTrack,
      cueKeys: new Set(),
    });

    if (!supported) {
      this.onStatus(
        `subtitle track ${track.index} (${format}) is not rendered in this demo`,
      );
    }

    this.emitTracks();
    if (this.activeIndex === null && supported) {
      this.setActiveTrack(track.index);
    }
    return true;
  }

  enqueueFragment(
    trackIndex: number,
    kind: string,
    bytes: Uint8Array,
    presentationTimeS?: number,
  ): boolean {
    const sink = this.sinks.get(trackIndex);
    if (!sink) {
      return false;
    }
    if (kind === "init" || !sink.supported) {
      return true;
    }

    const cues = cuesFromSubtitleBytes(
      sink.format,
      bytes,
      presentationTimeS ?? 0,
    );
    for (const cue of cues) {
      const key = cueKey(cue.start, cue.end, cue.text);
      if (sink.cueKeys.has(key)) {
        continue;
      }
      sink.cueKeys.add(key);
      try {
        sink.textTrack.addCue(new VTTCue(cue.start, cue.end, cue.text));
      } catch (err) {
        console.warn("Failed to add subtitle cue", err);
      }
    }
    return true;
  }

  setActiveTrack(trackIndex: number | null): void {
    this.activeIndex = trackIndex;
    for (const [index, sink] of this.sinks) {
      if (!sink.supported) {
        sink.textTrack.mode = "disabled";
        continue;
      }
      sink.textTrack.mode =
        trackIndex !== null && index === trackIndex ? "showing" : "disabled";
    }
  }

  listTracks(): SubtitleTrackOption[] {
    return [...this.sinks.values()].map((sink) => ({
      index: sink.info.index,
      label: trackLabel(sink.info, sink.format),
      language: sink.info.language,
      format: sink.format,
      supported: sink.supported,
    }));
  }

  private emitTracks(): void {
    this.onTracksChanged(this.listTracks());
  }

  private clearCues(sink: TextSink): void {
    const cues = sink.textTrack.cues;
    if (!cues) {
      return;
    }
    for (let i = cues.length - 1; i >= 0; i -= 1) {
      const cue = cues[i];
      if (cue) {
        sink.textTrack.removeCue(cue);
      }
    }
    sink.cueKeys.clear();
  }
}
