type BoxVisitor = (
  type: string,
  payloadStart: number,
  payloadEnd: number,
  view: DataView,
) => unknown;

/**
 * Walk ISO-BMFF boxes and invoke `visit(type, payloadStart, payloadEnd)`.
 * Return a value from `visit` to stop early.
 */
export function walkBoxes(
  bytes: Uint8Array,
  start: number,
  end: number,
  visit: BoxVisitor,
): unknown {
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
      bytes[offset + 4]!,
      bytes[offset + 5]!,
      bytes[offset + 6]!,
      bytes[offset + 7]!,
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

export function readMdhdTimescale(bytes: Uint8Array): number | undefined {
  const result = walkBoxes(bytes, 0, bytes.length, (type, payloadStart, payloadEnd, view) => {
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
  return typeof result === "number" ? result : undefined;
}

/** Read `baseMediaDecodeTime` from the first `tfdt` in an fMP4 fragment. */
export function readTfdtTiming(
  bytes: Uint8Array,
  timescaleHint: number,
): { decodeTime: number; timescale: number } | null {
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
  if (decodeTime === null || decodeTime === undefined || typeof decodeTime !== "number") {
    return null;
  }
  const timescale = timescaleHint > 0 ? timescaleHint : 1;
  return { decodeTime, timescale };
}

export function isInitSegment(bytes: Uint8Array): boolean {
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
      bytes[offset + 4]!,
      bytes[offset + 5]!,
      bytes[offset + 6]!,
      bytes[offset + 7]!,
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
