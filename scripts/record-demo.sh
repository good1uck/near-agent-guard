#!/usr/bin/env bash
# Record the testnet demo as a terminal video for the X post.
# Needs: asciinema (pip install asciinema), agg (github.com/asciinema/agg releases), ffmpeg.
# Output: demo/demo-testnet.cast, demo/demo.gif, demo/demo-video.mp4
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p demo
asciinema rec -q --overwrite --cols 120 --rows 42 -c "bash scripts/demo-testnet.sh" demo/demo-testnet.cast
agg --idle-time-limit 1.2 --speed 1.25 --font-size 18 --theme monokai --last-frame-duration 6 \
  demo/demo-testnet.cast demo/demo.gif
ffmpeg -y -loglevel error -i demo/demo.gif -movflags +faststart -pix_fmt yuv420p \
  -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2,fps=15" -c:v libx264 -crf 23 demo/demo-video.mp4
echo "video: demo/demo-video.mp4"
