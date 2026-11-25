#!/bin/sh
# Prototype 3 on Linux without a screen: the program on a virtual X (Xvfb), stopped after LIMIT seconds if it does not
# end itself (--bench and --shot end it). XSHOT=out.png: the whole X screen after SHOT seconds (every window, the
# detached ones too; needs xwd and python3, proto/gui-web/tauri/xwd2png.py).
#   ./run.sh --synth 1e8 --bench out.txt
# Environment: LIMIT (default 900), DISPLAY_NO (default 89), BIN (default the release build).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${BIN:-$here/target/release/wsk-iced}
dn=${DISPLAY_NO:-89}
Xvfb ":$dn" -screen 0 1600x960x24 -nolisten tcp >/dev/null 2>&1 &
xpid=$!
trap 'kill $xpid 2>/dev/null || true' EXIT
sleep 1
env DISPLAY=":$dn" WAYLAND_DISPLAY= "$bin" "$@" &
apid=$!
if [ -n "${XSHOT:-}" ]; then
  (sleep "${SHOT:-15}" && DISPLAY=":$dn" xwd -root -silent > "$XSHOT.xwd" && python3 "$here/../gui-web/tauri/xwd2png.py" "$XSHOT.xwd" "$XSHOT" && rm -f "$XSHOT.xwd") &
fi
n=0
while kill -0 $apid 2>/dev/null; do
  n=$((n + 1))
  [ $n -gt "${LIMIT:-900}" ] && { echo "[run.sh] stopped after ${LIMIT:-900} s"; kill $apid; break; }
  sleep 1
done
wait $apid 2>/dev/null || true
