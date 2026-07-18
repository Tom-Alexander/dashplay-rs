export type TrackInfo = {
  index: number;
  mime_type?: string | null;
  codecs: string[];
  kind: string;
  language?: string | null;
  is_dynamic: boolean;
  subtitle_type?: string | null;
  roles?: string[];
};

export type TrackSink = {
  mime: string;
  sourceBuffer: SourceBuffer;
  queue: Uint8Array[];
  appending: boolean;
  timestampOffsetReady: boolean;
  timescale: number;
};

export type PlaybackOptions = {
  licenseUrl?: string;
  deviceFile?: File;
  /** When false, buffer media but do not call play(). Defaults to true. */
  autoplay?: boolean;
};

export type SubtitleTrackOption = {
  index: number;
  label: string;
  language?: string | null;
  format: string;
  supported: boolean;
};

export type PlayerCallbacks = {
  onStatus: (message: string) => void;
  onError: (message: string) => void;
  onSubtitleTracks?: (tracks: SubtitleTrackOption[]) => void;
};

export type PlaybackHandle = {
  stop: () => void;
  setSubtitleTrack: (trackIndex: number | null) => void;
};
