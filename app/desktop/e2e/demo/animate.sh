#!/bin/sh
# The README's demo animation from the frames screenshots.py took:
# docs/images/demo.webp (animated WebP) and docs/images/demo.gif (fallback).
# Needs ffmpeg with libwebp. Budget: under 5 MB each.
set -eu
cd "$(dirname "$0")/../../../.."
F=app/desktop/e2e/out/frames
O=docs/images
[ -f "$F/frames.txt" ] || { echo "no frames: run screenshots.py first" >&2; exit 1; }
ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i "$F/frames.txt" \
  -vf "fps=10,scale=960:-2:flags=lanczos" -c:v libwebp_anim -lossless 0 -quality 72 -compression_level 6 -loop 0 "$O/demo.webp"
ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i "$F/frames.txt" \
  -vf "fps=5,scale=800:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
  -loop 0 "$O/demo.gif"
for f in "$O/demo.webp" "$O/demo.gif"; do
  kb=$(( $(wc -c < "$f") / 1024 ))
  echo "$f: $kb KB"
  [ "$kb" -le 5120 ] || { echo "$f is over its 5 MB budget" >&2; exit 1; }
done
