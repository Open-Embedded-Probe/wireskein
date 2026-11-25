#!/bin/sh
# Prototype 2b on Linux without a screen: the shell started on a virtual X (Xvfb), a page query run (the bench exits
# the app when it is done with `exit=1`), the reports printed; a screenshot of the screen after SHOT seconds.
#   ./bench.sh 'file=files/examples/mixed-1.wireskein&engine=native&ipc=raw&bench=1&report=1&exit=1' [out.png]
# Environment: SHOT (seconds before the screenshot, default 12), LIMIT (seconds before it is stopped, default 900),
# ARGS (more for the shell: --mount scratch=/path/to/dir), DISPLAY_NO (default 87), BIN (the shell, default the
# release build). The screenshot needs xwd and python3.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
q=$1
png=${2:-}
bin=${BIN:-$here/target/release/wsk-gui-tauri}
dn=${DISPLAY_NO:-87}
Xvfb ":$dn" -screen 0 1600x960x24 -nolisten tcp >/dev/null 2>&1 &
xpid=$!
trap 'kill $xpid 2>/dev/null || true' EXIT
sleep 1
cd "$here/../web"
# shellcheck disable=SC2086
env DISPLAY=":$dn" WEBKIT_DISABLE_DMABUF_RENDERER=1 ${WEBKIT_ENV:-} "$bin" --size 1600x960 ${ARGS:-} "$q" &
apid=$!
if [ -n "$png" ]; then
  (sleep "${SHOT:-12}" && DISPLAY=":$dn" xwd -root -silent > "$png.xwd" && python3 "$here/xwd2png.py" "$png.xwd" "$png" && rm -f "$png.xwd") &
fi
n=0
while kill -0 $apid 2>/dev/null; do
  n=$((n + 1))
  [ $n -gt "${LIMIT:-900}" ] && { echo "[bench.sh] stopped after ${LIMIT:-900} s"; kill $apid; break; }
  sleep 1
done
wait $apid 2>/dev/null || true
