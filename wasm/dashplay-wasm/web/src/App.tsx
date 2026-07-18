import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { startPlayback } from "./mse/player";
import { Readme } from "./Readme";
import {
  applyTheme,
  getInitialTheme,
  persistTheme,
  type Theme,
} from "./theme";
import type { SubtitleTrackOption } from "./types";

const MANIFESTS = [
  {
    label: "Default",
    url: "https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd",
  },
  {
    label: "LL-DASH",
    url: "https://cmafref.akamaized.net/cmaf/live-ull/2006350/akambr/out.mpd",
  },
  {
    label: "Subtitles",
    url: "https://dash.akamaized.net/akamai/test/caption_test/ElephantsDream/elephants_dream_480p_heaac5_1_https.mpd",
  },
  {
    label: "DRM",
    url: "https://storage.googleapis.com/shaka-demo-assets/angel-one-widevine/dash.mpd",
  },
] as const;

const DEFAULT_MANIFEST = MANIFESTS[0].url;

const inputClassName =
  "rounded-lg border border-neutral-300 bg-white px-3 py-2 text-neutral-900 outline-none focus:border-primary dark:border-neutral-700 dark:bg-neutral-900 dark:text-neutral-100";

export default function App() {
  const videoRef = useRef<HTMLVideoElement>(null);
  const stopRef = useRef<(() => void) | null>(null);
  const setSubtitleTrackRef = useRef<((trackIndex: number | null) => void) | null>(
    null,
  );

  const [manifestUrl, setManifestUrl] = useState<string>(DEFAULT_MANIFEST);
  const [licenseUrl, setLicenseUrl] = useState(
    "https://cwip-shaka-proxy.appspot.com/no_auth",
  );
  const [deviceFile, setDeviceFile] = useState<File | undefined>();
  const [error, setError] = useState("");
  const [playing, setPlaying] = useState(false);
  const [theme, setTheme] = useState<Theme>(() => getInitialTheme());
  const [subtitleTracks, setSubtitleTracks] = useState<SubtitleTrackOption[]>([]);
  const [selectedSubtitle, setSelectedSubtitle] = useState<string>("");

  useEffect(() => {
    applyTheme(theme);
    persistTheme(theme);
  }, [theme]);

  const toggleTheme = useCallback(() => {
    setTheme((current) => (current === "dark" ? "light" : "dark"));
  }, []);

  const onSubtitleChange = useCallback(
    (value: string) => {
      setSelectedSubtitle(value);
      const trackIndex = value === "" ? null : Number(value);
      setSubtitleTrackRef.current?.(
        trackIndex !== null && Number.isFinite(trackIndex) ? trackIndex : null,
      );
    },
    [],
  );

  const loadManifest = useCallback(
    async (autoplay: boolean) => {
      const video = videoRef.current;
      if (!video) {
        return;
      }

      stopRef.current?.();
      stopRef.current = null;
      setSubtitleTrackRef.current = null;
      setSubtitleTracks([]);
      setSelectedSubtitle("");
      setError("");
      setPlaying(true);

      try {
        const handle = await startPlayback(
          video,
          manifestUrl.trim(),
          {
            licenseUrl: licenseUrl.trim() || undefined,
            deviceFile,
            autoplay,
          },
          {
            onStatus: (status) => {
              console.log("status", status);
            },
            onError: (message) => {
              setError(message);
              if (message) {
                setPlaying(false);
              }
            },
            onSubtitleTracks: (tracks) => {
              setSubtitleTracks(tracks);
              const preferred =
                tracks.find((track) => track.supported)?.index ?? null;
              if (preferred !== null) {
                setSelectedSubtitle(String(preferred));
              }
            },
          },
        );
        stopRef.current = handle.stop;
        setSubtitleTrackRef.current = handle.setSubtitleTrack;
      } catch (err) {
        setError(String(err));
        setPlaying(false);
      }
    },
    [deviceFile, licenseUrl, manifestUrl],
  );

  useEffect(() => {
    void loadManifest(false);
    return () => {
      stopRef.current?.();
      stopRef.current = null;
    };
    // Initial load of the default manifest only.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-only
  }, []);

  const onSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      void loadManifest(true);
    },
    [loadManifest],
  );

  return (
    <div className="mx-auto max-w-3xl px-6 py-8">
      <div className="mb-6 flex items-center justify-between gap-4">
        <h1 className="text-xl font-semibold tracking-tight">
          dashplay-<span className="text-primary">rs</span>
        </h1>
        <div className="flex items-center gap-3">
          <a
            href="https://docs.rs/dashplay/latest/dashplay/"
            target="_blank"
            rel="noopener noreferrer"
            className="text-sm text-neutral-600 transition hover:text-neutral-900 dark:text-neutral-400 dark:hover:text-neutral-100"
          >
            Docs
          </a>
          <a
            href="https://github.com/Tom-Alexander/dashplay-rs"
            target="_blank"
            rel="noopener noreferrer"
            className="text-sm text-neutral-600 transition hover:text-neutral-900 dark:text-neutral-400 dark:hover:text-neutral-100"
          >
            GitHub
          </a>
          <button
            type="button"
            onClick={toggleTheme}
            aria-label={
              theme === "dark" ? "Switch to light mode" : "Switch to dark mode"
            }
            className="rounded-lg border border-neutral-300 p-2 text-neutral-600 transition hover:bg-neutral-100 hover:text-neutral-900 dark:border-neutral-700 dark:text-neutral-400 dark:hover:bg-neutral-900 dark:hover:text-neutral-100"
          >
            {theme === "dark" ? <SunIcon /> : <MoonIcon />}
          </button>
        </div>
      </div>

      <video
        ref={videoRef}
        controls
        playsInline
        className="mb-3 min-h-48 w-full rounded-lg bg-black"
      />
      <form className="mt-6" onSubmit={(e) => void onSubmit(e)}>
        <details>
          <summary className="cursor-pointer text-sm text-neutral-600 dark:text-neutral-300">
            Stream settings
          </summary>
          <div className="mt-4 grid gap-6">
            <label className="grid gap-1.5 text-sm text-neutral-600 dark:text-neutral-300">
              Manifest
              <select
                required
                value={manifestUrl}
                onChange={(e) => setManifestUrl(e.target.value)}
                className={inputClassName}
              >
                {MANIFESTS.map((manifest) => (
                  <option key={manifest.url} value={manifest.url}>
                    {manifest.label}
                  </option>
                ))}
              </select>
            </label>

            {subtitleTracks.length > 0 ? (
              <label className="grid gap-1.5 text-sm text-neutral-600 dark:text-neutral-300">
                Subtitles
                <select
                  value={selectedSubtitle}
                  onChange={(e) => onSubtitleChange(e.target.value)}
                  className={inputClassName}
                >
                  <option value="">Off</option>
                  {subtitleTracks.map((track) => (
                    <option
                      key={track.index}
                      value={track.index}
                      disabled={!track.supported}
                    >
                      {track.label}
                      {track.supported ? "" : " (unsupported)"}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}

            <label className="grid gap-1.5 text-sm text-neutral-600 dark:text-neutral-300">
              License URL (optional override)
              <input
                type="url"
                value={licenseUrl}
                onChange={(e) => setLicenseUrl(e.target.value)}
                placeholder="from MPD ContentProtection when empty"
                className={`${inputClassName} placeholder:text-neutral-400 dark:placeholder:text-neutral-600`}
              />
            </label>

            <label className="grid gap-1.5 text-sm text-neutral-600 dark:text-neutral-300">
              Widevine device (.wvd) — required for encrypted streams
              <input
                type="file"
                accept=".wvd,application/octet-stream"
                onChange={(e) => setDeviceFile(e.target.files?.[0] ?? undefined)}
                className={`${inputClassName} text-sm file:mr-3 file:rounded file:border-0 file:bg-neutral-200 file:px-3 file:py-1 file:text-neutral-700 dark:file:bg-neutral-800 dark:file:text-neutral-200`}
              />
            </label>

            <button
              type="submit"
              disabled={playing && !error}
              className="w-fit rounded-lg bg-primary px-4 py-2.5 text-sm font-medium text-white transition hover:bg-primary-hover disabled:cursor-not-allowed disabled:opacity-50"
            >
              Load Stream
            </button>
          </div>
        </details>
      </form>
      <Readme />
      <footer className="mt-12 border-t border-neutral-200 pt-6 text-sm text-neutral-500 dark:border-neutral-800 dark:text-neutral-500">
        &copy; 2026 Tom Alexander
      </footer>
    </div>
  );
}

function SunIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="size-4"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="size-4"
      aria-hidden="true"
    >
      <path d="M21 14.5A8.5 8.5 0 1 1 9.5 3a7 7 0 0 0 11.5 11.5z" />
    </svg>
  );
}
