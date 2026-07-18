export type SubtitleFormat = "vtt" | "ttml" | "stpp" | "wvtt" | "unknown" | string;

export type Cue = {
  start: number;
  end: number;
  text: string;
};

const TIMESTAMP =
  /^(?:(\d{2,}):)?(\d{2}):(\d{2})(?:[.,](\d{1,3})|:(\d{2}))?$/;

/** Parse a WebVTT / TTML clock value into seconds. */
export function parseTimestamp(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  const clock = TIMESTAMP.exec(trimmed);
  if (clock) {
    const hours = clock[1] ? Number(clock[1]) : 0;
    const minutes = Number(clock[2]);
    const seconds = Number(clock[3]);
    const millis = clock[4] ? Number(clock[4].padEnd(3, "0")) : 0;
    const frames = clock[5] ? Number(clock[5]) : 0;
    // Treat bare :FF as 0 ms when no frame rate is available.
    return hours * 3600 + minutes * 60 + seconds + millis / 1000 + frames / 30;
  }

  const metric = /^(\d+(?:\.\d+)?)(h|m|s|ms)?$/i.exec(trimmed);
  if (metric) {
    const amount = Number(metric[1]);
    switch ((metric[2] ?? "s").toLowerCase()) {
      case "h":
        return amount * 3600;
      case "m":
        return amount * 60;
      case "ms":
        return amount / 1000;
      default:
        return amount;
    }
  }

  return null;
}

function stripMarkup(text: string): string {
  return text
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/?[^>]+>/g, "")
    .replace(/&nbsp;/g, " ")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&")
    .replace(/&quot;/g, '"')
    .replace(/\r/g, "")
    .trim();
}

function decodeUtf8(bytes: Uint8Array): string {
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

/** Parse a WebVTT document (full file or DASH segment) into cues. */
export function parseWebVtt(source: string): Cue[] {
  const normalized = source.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const blocks = normalized.split(/\n\n+/);
  const cues: Cue[] = [];

  for (const block of blocks) {
    const lines = block
      .split("\n")
      .map((line) => line.trimEnd())
      .filter((line, index, all) => !(index === 0 && all.length > 1 && line === ""));
    if (lines.length === 0) {
      continue;
    }

    const first = lines[0]!.trim();
    if (
      /^WEBVTT/i.test(first) ||
      /^NOTE\b/i.test(first) ||
      /^STYLE\b/i.test(first) ||
      /^REGION\b/i.test(first)
    ) {
      continue;
    }

    let timingIndex = 0;
    if (!first.includes("-->") && lines.length > 1 && lines[1]!.includes("-->")) {
      timingIndex = 1;
    }
    const timingLine = lines[timingIndex];
    if (!timingLine?.includes("-->")) {
      continue;
    }

    const [startRaw, endRaw] = timingLine.split("-->").map((part) => part.trim().split(/\s+/)[0] ?? "");
    const start = parseTimestamp(startRaw);
    const end = parseTimestamp(endRaw);
    if (start === null || end === null || end <= start) {
      continue;
    }

    const text = stripMarkup(lines.slice(timingIndex + 1).join("\n"));
    if (!text) {
      continue;
    }
    cues.push({ start, end, text });
  }

  return cues;
}

function ttmlTime(el: Element, name: string): number | null {
  const value = el.getAttribute(name);
  return value ? parseTimestamp(value) : null;
}

/** Parse TTML / IMSC XML into cues. */
export function parseTtml(source: string, timeOffset = 0): Cue[] {
  const xmlStart = source.search(/<\?xml|<tt[\s>]/i);
  const xml = xmlStart >= 0 ? source.slice(xmlStart) : source;
  const doc = new DOMParser().parseFromString(xml, "application/xml");
  if (doc.querySelector("parsererror")) {
    return [];
  }

  const cues: Cue[] = [];
  const paragraphs = doc.getElementsByTagNameNS("*", "p");
  for (let i = 0; i < paragraphs.length; i += 1) {
    const el = paragraphs.item(i);
    if (!el) {
      continue;
    }
    let start = ttmlTime(el, "begin") ?? ttmlTime(el, "start");
    let end = ttmlTime(el, "end");
    const dur = ttmlTime(el, "dur");
    if (start === null) {
      continue;
    }
    if (end === null && dur !== null) {
      end = start + dur;
    }
    if (end === null || end <= start) {
      continue;
    }
    const text = stripMarkup(el.textContent ?? "");
    if (!text) {
      continue;
    }
    cues.push({
      start: start + timeOffset,
      end: end + timeOffset,
      text,
    });
  }
  return cues;
}

/** Collect top-level `mdat` payloads from an fMP4 fragment. */
export function extractMdatPayloads(bytes: Uint8Array): Uint8Array[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const payloads: Uint8Array[] = [];
  let offset = 0;

  while (offset + 8 <= bytes.length) {
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
    if (size < header || offset + size > bytes.length) {
      break;
    }

    const type = String.fromCharCode(
      bytes[offset + 4]!,
      bytes[offset + 5]!,
      bytes[offset + 6]!,
      bytes[offset + 7]!,
    );
    if (type === "mdat") {
      payloads.push(bytes.subarray(offset + header, offset + size));
    }
    offset += size;
  }

  return payloads;
}

function extractXmlDocuments(payload: Uint8Array): string[] {
  const text = decodeUtf8(payload);
  const docs: string[] = [];
  const re = /(?:<\?xml[\s\S]*?)?<tt\b[\s\S]*?<\/(?:tt:)?tt>/gi;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    docs.push(match[0]);
  }
  if (docs.length === 0 && /<tt\b/i.test(text)) {
    docs.push(text);
  }
  return docs;
}

export function cuesFromSubtitleBytes(
  format: SubtitleFormat,
  bytes: Uint8Array,
  presentationTimeS = 0,
): Cue[] {
  const text = decodeUtf8(bytes);

  switch (format) {
    case "vtt":
      return parseWebVtt(text);
    case "ttml":
      return parseTtml(text);
    case "stpp": {
      const payloads = extractMdatPayloads(bytes);
      const sources = payloads.length > 0 ? payloads : [bytes];
      const cues: Cue[] = [];
      for (const payload of sources) {
        for (const xml of extractXmlDocuments(payload)) {
          cues.push(...parseTtml(xml, presentationTimeS));
        }
      }
      return cues;
    }
    default:
      if (/^\s*WEBVTT/i.test(text)) {
        return parseWebVtt(text);
      }
      if (/<tt\b/i.test(text)) {
        return parseTtml(text);
      }
      return [];
  }
}
