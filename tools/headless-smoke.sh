#!/usr/bin/env bash
# Run inside archlinux:latest with /work read-only and /artifacts writable.
set -euo pipefail

if [[ ${1:-} != --session ]]; then
  mkdir -p /artifacts
  pacman -Syu --noconfirm --needed hyprland mesa libglvnd freerdp \
    xorg-server-xvfb xorg-xdpyinfo imagemagick foot ttf-dejavu python python-pillow \
    > /artifacts/packages.log 2>&1
  useradd --create-home smoke
  chown smoke:smoke /artifacts
  pacman -Q > /artifacts/versions.log
  if [[ -e /dev/dri ]]; then
    echo 'Unexpected /dev/dri; this test requires no GPU devices.' >&2
    exit 1
  fi
  echo 'No /dev/dri devices are available.' > /artifacts/devices.log
  exec runuser -u smoke -- env -i HOME=/home/smoke USER=smoke PATH=/usr/bin:/bin \
    GITHUB_ACTIONS="${GITHUB_ACTIONS:-false}" \
    LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
    AQ_BACKENDS=headless AQ_NO_KMS_REQUIREMENT=1 HYPRLAND_NO_RT=1 \
    bash /work/tools/headless-smoke.sh --session
fi

umask 077
ulimit -c 0
# Hyprland includes its instance signature in Unix socket paths, limited to 108 bytes.
session_dir=$(mktemp -d /tmp/ws.XXXXXX)
export XDG_RUNTIME_DIR="$session_dir/r"
export XDG_CONFIG_HOME="$session_dir/config"
export XDG_CACHE_HOME="$session_dir/cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
export DISPLAY=:99
export RUST_LOG=info,wayshare::rdp=debug
binary=/work/target/debug/wayshare
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  mkdir -p /artifacts/compositor
  find "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" -type f \
    \( -name '*log*' -o -name 'hyprlandCrashReport*.txt' \) \
    -exec cp --backup=numbered '{}' /artifacts/compositor/ \; 2>/dev/null || true
  rm -rf "$session_dir"
  chmod -R a+rX /artifacts
}
trap cleanup EXIT

password=$(python -c 'import secrets; print(secrets.token_hex(24))')
if [[ $GITHUB_ACTIONS == true ]]; then printf '::add-mask::%s\n' "$password"; fi
printf '%s\n' "$password" > "$session_dir/password"
unset password
"$binary" set-password --user smoke --password-file "$session_dir/password" \
  > /artifacts/setup.log 2>&1

Xvfb "$DISPLAY" -screen 0 1024x768x24 -nolisten tcp > /artifacts/xvfb.log 2>&1 &
pids+=("$!")
for _ in {1..50}; do
  if xdpyinfo > /dev/null 2>&1; then break; fi
  sleep 0.1
done
xdpyinfo > /artifacts/xdisplay.log

cat > "$session_dir/hyprland.lua" <<'CONF'
hl.monitor({ output = "HEADLESS-1", mode = "800x600@30", position = "0x0", scale = 1 })
hl.config({ debug = { disable_logs = false, enable_stdout_logs = true } })
hl.on("hyprland.start", function()
    hl.exec_cmd("hyprctl output create headless HEADLESS-1")
end)
CONF
Hyprland --config "$session_dir/hyprland.lua" > /artifacts/hyprland.log 2>&1 &
hypr_pid=$!
pids+=("$hypr_pid")
hypr_ready=false
for _ in {1..100}; do
  if ! kill -0 "$hypr_pid" 2>/dev/null; then break; fi
  instance=$(find "$XDG_RUNTIME_DIR/hypr" -name .socket.sock -print -quit 2>/dev/null || true)
  if [[ -n $instance ]]; then
    export HYPRLAND_INSTANCE_SIGNATURE
    HYPRLAND_INSTANCE_SIGNATURE=$(basename "$(dirname "$instance")")
    wayland_socket=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
    if [[ -n $wayland_socket ]] && hyprctl -j monitors > /artifacts/monitors.json 2>&1; then
      export WAYLAND_DISPLAY
      WAYLAND_DISPLAY=$(basename "$wayland_socket")
      hypr_ready=true
      break
    fi
  fi
  sleep 0.1
done

# The screenshot must contain the terminal's teal background, not just a client window.
cat > "$session_dir/visible-client.sh" <<'CLIENT'
#!/usr/bin/env bash
printf '\n\n  wayshare headless CI\n\n  This terminal came through Hyprland and RDP.\n'
while true; do printf '\r  Frame time: %s  ' "$(date +%T)"; sleep 1; done
CLIENT
if [[ $hypr_ready == true ]]; then
  hyprctl output create headless HEADLESS-1 > /artifacts/output.log 2>&1 || true
  foot --app-id=wayshare-smoke --override=colors-dark.background=164e63 \
    --override=colors-dark.foreground=ffffff --font='DejaVu Sans Mono:size=18' \
    bash "$session_dir/visible-client.sh" > /artifacts/foot.log 2>&1 &
  pids+=("$!")
  sleep 1
  hyprctl -j clients > /artifacts/clients.json
fi

receive_frame() {
  local mode=$1 port=$2 client_pid received=false
  timeout 25s xfreerdp3 /v:"127.0.0.1:$port" /u:smoke /sec:tls \
    /cert:ignore /from-stdin:force /size:800x600 /gfx:AVC420 -decorations \
    < "$session_dir/password" > "/artifacts/freerdp-$mode.log" 2>&1 &
  client_pid=$!
  pids+=("$client_pid")
  for _ in {1..15}; do
    sleep 1
    magick import -window root "/artifacts/received-$mode.png"
    if python - "/artifacts/received-$mode.png" "$mode" <<'PY'
import sys
from PIL import Image

image = Image.open(sys.argv[1]).convert("RGB")
pixels = list(image.get_flattened_data())
if sys.argv[2] == "hyprland":
    matching = sum(max(abs(c - t) for c, t in zip(p, (22, 78, 99))) < 16 for p in pixels)
    ok = matching > 10000
else:
    ok = len(set(pixels)) > 1000 and sum(max(p) - min(p) > 50 for p in pixels) > 10000
sys.exit(0 if ok else 1)
PY
    then
      if grep -q 'RDP EGFX frame ACK' "/artifacts/wayshare-$mode.log"; then
        received=true
        break
      fi
    fi
    if ! kill -0 "$client_pid" 2>/dev/null; then break; fi
  done
  kill "$client_pid" 2>/dev/null || true
  wait "$client_pid" 2>/dev/null || true
  [[ $received == true ]]
}

# Keep the real desktop result separate from the synthetic transport control.
"$binary" --output HEADLESS-1 --listen 127.0.0.1:15900 --rdp-listen 127.0.0.1:13389 \
  --no-mdns --no-hp --no-resize --no-exclusive \
  > /artifacts/wayshare-hyprland.log 2>&1 &
server_pid=$!
pids+=("$server_pid")
sleep 1
desktop_result=FAIL
if receive_frame hyprland 13389 && [[ $hypr_ready == true ]]; then desktop_result=PASS; fi
kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true

"$binary" --test-pattern --listen 127.0.0.1:15901 --rdp-listen 127.0.0.1:13390 \
  --no-mdns --no-hp --no-input --no-resize --no-exclusive \
  > /artifacts/wayshare-pattern.log 2>&1 &
server_pid=$!
pids+=("$server_pid")
sleep 1
pattern_result=FAIL
if receive_frame pattern 13390; then pattern_result=PASS; fi
kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true

printf 'Hyprland desktop capture: %s\nRDP test-pattern control: %s\n' \
  "$desktop_result" "$pattern_result" | tee /artifacts/result.txt
[[ $desktop_result == PASS && $pattern_result == PASS ]]
