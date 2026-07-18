#!/usr/bin/env bash
# Generate CENC-encrypted fMP4 media for tests/fixtures/dashif_drm_encrypted/.
#
# Requires: ffmpeg, mp4fragment, mp4split, mp4encrypt (Bento4).
#
# Optional license capture (for full Widevine playback tests):
#   DEVICE_PATH=/path/to/device.wvd WV_LICENSE_URL=https://license.example/wv ./scripts/generate-dashif-drm-fixture.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/tests/fixtures/dashif_drm_encrypted"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

KID=eb6769950da145d03ae4082255eb141a
KEY=00112233445566778899aabbccddeeff
IV=0000000000000000
PSSH_B64='AAAANHBzc2gAAAAA7e+LqXnWSs6jyCfc1R0h7QAAABsIARIQ62dplQ2hRdA65AgiVesUGg=='

mkdir -p "$OUT"

echo "Generating clear fragmented MP4 in $WORK ..."
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i color=c=red:s=320x240:d=4 \
  -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
  -movflags +frag_keyframe+empty_moov+default_base_moof \
  "$WORK/clear.mp4"

mp4fragment --fragment-duration 2000 "$WORK/clear.mp4" "$WORK/fragmented.mp4"
(
  cd "$WORK"
  mp4split fragmented.mp4
)

echo "Encrypting init + media segments ..."
mp4encrypt --method MPEG-CENC \
  --key "1:${KEY}:${IV}" --property "1:KID:${KID}" \
  "$WORK/init.mp4" "$OUT/init.mp4"

shopt -s nullglob
for seg in "$WORK"/segment-*.m4s; do
  base="$(basename "$seg")"
  mp4encrypt --method MPEG-CENC \
    --key "1:${KEY}:${IV}" --property "1:KID:${KID}" \
    "$seg" "$OUT/$base"
done

cat > "$OUT/manifest.mpd" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     xmlns:cenc="urn:mpeg:cenc:2013"
     xmlns:mspr="urn:microsoft:playready"
     profiles="urn:mpeg:dash:profile:isoff-on-demand:2011,http://dashif.org/guidelines/dash264"
     type="static"
     mediaPresentationDuration="PT4S"
     minBufferTime="PT2S">
  <ProgramInformation>
    <Title>DASH-IF encrypted CENC vector (local, KID ${KID})</Title>
  </ProgramInformation>
  <Period id="p0">
    <AdaptationSet mimeType="video/mp4" contentType="video" segmentAlignment="true"
                   par="16:9" maxWidth="320" maxHeight="240" maxFrameRate="25">
      <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed" value="Widevine">
        <cenc:pssh>${PSSH_B64}</cenc:pssh>
        <mspr:laurl>https://license.example/wv</mspr:laurl>
      </ContentProtection>
      <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>
      <SegmentTemplate timescale="1000" duration="2000" initialization="init.mp4"
                       media="segment-1.\$Number%04d\$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="320" height="240"
                    sar="1:1" frameRate="25"/>
    </AdaptationSet>
  </Period>
</MPD>
EOF

echo "Wrote encrypted media and manifest to $OUT"

if [[ -n "${DEVICE_PATH:-}" && -n "${WV_LICENSE_URL:-}" ]]; then
  echo "Capturing Widevine license response to $OUT/license-response.bin ..."
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" --example capture_widevine_license -- \
    --pssh-b64 "$PSSH_B64" \
    --license-url "$WV_LICENSE_URL" \
    --output "$OUT/license-response.bin"
  echo "License response captured."
else
  echo "Skip license capture (set DEVICE_PATH and WV_LICENSE_URL to capture license-response.bin)."
fi
