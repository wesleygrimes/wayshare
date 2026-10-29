#!/usr/bin/env bash
# Session phases run inside the container; device discovery also runs on the runner.
set -euo pipefail

phase=${1:?pass prepare, start-hyprland, start-server, freerdp, collect, or --list-software-devices}
session_dir=/tmp/ws
artifacts=/artifacts

drm_driver() {
  local device driver
  device=$(readlink -f "/sys/class/drm/$(basename "$1")/device")
  driver=$(basename "$(readlink -f "$device/driver")")
  # These modules use faux devices on recent kernels, without driver symlinks.
  case $(basename "$device") in vkms|vgem) driver=$(basename "$device") ;; esac
  printf '%s\n' "$driver"
}

if [[ $phase == --list-software-devices ]]; then
  for node in /dev/dri/*; do
    [[ -c $node ]] || continue
    case $(drm_driver "$node") in vkms|vgem) printf '%s\n' "$node" ;; esac
  done
  exit 0
fi

if [[ $phase == prepare ]]; then
  : "${DESKTOP_SESSION_USER:?}" "${DESKTOP_SESSION_SIZE:?}" "${DESKTOP_SESSION_PORT:?}"
  [[ $DESKTOP_SESSION_SIZE =~ ^[0-9]+x[0-9]+$ ]]
  pacman -Syu --noconfirm --needed hyprland mesa mesa-utils libglvnd freerdp seatd \
    xorg-server-xvfb xorg-xdpyinfo imagemagick foot ttf-dejavu python python-pillow \
    > "$artifacts/packages.log" 2>&1
  useradd --create-home "$DESKTOP_SESSION_USER"
  # Logs are shared by the runner user and the container user; secrets stay in /tmp/ws.
  chmod 1777 "$artifacts"
  install -d -m 700 -o "$DESKTOP_SESSION_USER" -g "$DESKTOP_SESSION_USER" \
    "$session_dir" "$session_dir/r" "$session_dir/config" "$session_dir/cache"
  install -m 600 -o "$DESKTOP_SESSION_USER" -g "$DESKTOP_SESSION_USER" \
    /run/desktop-session-password "$session_dir/password"
  pacman -Q > "$artifacts/versions.log"
  chmod 755 /dev/dri
  drm_cards=()
  for node in /dev/dri/*; do
    driver=$(drm_driver "$node")
    case "$driver" in
      vkms|vgem) ;;
      *) echo "Unexpected DRM driver: $driver" >&2; exit 1 ;;
    esac
    chown "$DESKTOP_SESSION_USER:$DESKTOP_SESSION_USER" "$node"
    if [[ $(basename "$node") == card* ]]; then drm_cards+=("$node"); fi
  done
  chown "$DESKTOP_SESSION_USER:$DESKTOP_SESSION_USER" /dev/udmabuf
  ls -ld /dev/dri /dev/dri/* /dev/udmabuf > "$artifacts/devices.log"
  nohup env SEATD_VTBOUND=0 seatd -u "$DESKTOP_SESSION_USER" \
    > "$artifacts/seatd.log" 2>&1 < /dev/null &
  for _ in {1..50}; do [[ -S /run/seatd.sock ]] && break; sleep 0.1; done
  [[ -S /run/seatd.sock ]]
  drm_list=$(IFS=:; echo "${drm_cards[*]}")
  {
    printf 'export DESKTOP_SESSION_USER=%q\n' "$DESKTOP_SESSION_USER"
    printf 'export DESKTOP_SESSION_SIZE=%q\n' "$DESKTOP_SESSION_SIZE"
    printf 'export DESKTOP_SESSION_PORT=%q\n' "$DESKTOP_SESSION_PORT"
    printf 'export AQ_DRM_DEVICES=%q\n' "$drm_list"
  } > "$session_dir/environment"
  chmod 600 "$session_dir/environment"
  chown "$DESKTOP_SESSION_USER:$DESKTOP_SESSION_USER" "$session_dir/environment"
  exit 0
fi

if [[ $phase == collect ]]; then
  mkdir -p "$artifacts/compositor"
  find "$session_dir/r" "$session_dir/cache" -type f \
    \( -name '*log*' -o -name 'hyprlandCrashReport*.txt' \) \
    -exec cp --backup=numbered '{}' "$artifacts/compositor/" \; 2>/dev/null || true
  chmod -R a+rX "$artifacts"
  exit 0
fi

# shellcheck source=/dev/null
source "$session_dir/environment"
if [[ ${2:-} != --user ]]; then
  exec runuser -u "$DESKTOP_SESSION_USER" -- env -i \
    HOME="/home/$DESKTOP_SESSION_USER" USER="$DESKTOP_SESSION_USER" PATH=/usr/bin:/bin LANG=C.UTF-8 \
    LIBSEAT_BACKEND=seatd LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
    AQ_NO_KMS_REQUIREMENT=1 HYPRLAND_NO_RT=1 bash /work/tools/headless-smoke.sh "$phase" --user
fi

ulimit -c 0
export XDG_RUNTIME_DIR="$session_dir/r" XDG_CONFIG_HOME="$session_dir/config" XDG_CACHE_HOME="$session_dir/cache"
export DISPLAY=:99 RUST_LOG=info,wayshare::rdp=debug
width=${DESKTOP_SESSION_SIZE%x*}
height=${DESKTOP_SESSION_SIZE#*x}
binary=/work/target/debug/wayshare
if [[ -f $session_dir/wayland ]]; then
  # shellcheck source=/dev/null
  source "$session_dir/wayland"
fi

case "$phase" in
  start-hyprland)
    eglinfo -B -p surfaceless > "$artifacts/egl.log" 2>&1 || true
    cat > "$session_dir/hyprland.lua" <<CONF
hl.monitor({ output = "HEADLESS-1", mode = "${width}x${height}@30", position = "0x0", scale = 1 })
hl.monitor({ output = "Virtual-1", disabled = true })
hl.monitor({ output = "Virtual-2", disabled = true })
hl.config({ debug = { disable_logs = false, enable_stdout_logs = true } })
hl.on("hyprland.start", function()
    hl.exec_cmd("hyprctl output create headless HEADLESS-1")
end)
CONF
    nohup Hyprland --config "$session_dir/hyprland.lua" > "$artifacts/hyprland.log" 2>&1 < /dev/null &
    hypr_pid=$!
    ready=false
    for _ in {1..200}; do
      kill -0 "$hypr_pid" 2>/dev/null || break
      instance=$(find "$XDG_RUNTIME_DIR/hypr" -name .socket.sock -print -quit 2>/dev/null || true)
      if [[ -n $instance ]]; then
        export HYPRLAND_INSTANCE_SIGNATURE
        HYPRLAND_INSTANCE_SIGNATURE=$(basename "$(dirname "$instance")")
        wayland_socket=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
        if [[ -n $wayland_socket ]] && hyprctl -j monitors > "$artifacts/monitors.json" 2>&1; then
          export WAYLAND_DISPLAY
          WAYLAND_DISPLAY=$(basename "$wayland_socket")
          ready=true
          break
        fi
      fi
      sleep 0.1
    done
    [[ $ready == true ]]
    {
      printf 'export HYPRLAND_INSTANCE_SIGNATURE=%q\n' "$HYPRLAND_INSTANCE_SIGNATURE"
      printf 'export WAYLAND_DISPLAY=%q\n' "$WAYLAND_DISPLAY"
    } > "$session_dir/wayland"
    hyprctl output create headless HEADLESS-1 > "$artifacts/output.log" 2>&1 || true
    hyprctl -j monitors > "$artifacts/monitors.json"
    cat > "$session_dir/visible-client.sh" <<'CLIENT'
#!/usr/bin/env bash
printf '\n\n  wayshare headless CI\n\n  This terminal came through Hyprland and RDP.\n'
while true; do printf '\r  Frame time: %s  ' "$(date +%T)"; sleep 1; done
CLIENT
    nohup foot --app-id=wayshare-smoke --override=colors-dark.background=164e63 \
      --override=colors-dark.foreground=ffffff --font='DejaVu Sans Mono:size=18' \
      bash "$session_dir/visible-client.sh" > "$artifacts/foot.log" 2>&1 < /dev/null &
    sleep 1
    hyprctl -j clients > "$artifacts/clients.json"
    ;;
  start-server)
    "$binary" set-password --user "$DESKTOP_SESSION_USER" --password-file "$session_dir/password" \
      > "$artifacts/setup.log" 2>&1
    nohup "$binary" --output HEADLESS-1 --listen 127.0.0.1:15900 \
      --rdp-listen "0.0.0.0:$DESKTOP_SESSION_PORT" \
      --no-mdns --no-hp --no-resize --no-exclusive \
      > "$artifacts/wayshare.log" 2>&1 < /dev/null &
    ;;
  freerdp)
    nohup Xvfb "$DISPLAY" -screen 0 "${width}x${height}x24" -nolisten tcp \
      > "$artifacts/xvfb.log" 2>&1 < /dev/null &
    for _ in {1..50}; do xdpyinfo > /dev/null 2>&1 && break; sleep 0.1; done
    xdpyinfo > "$artifacts/xdisplay.log"
    timeout 45s xfreerdp3 /v:"127.0.0.1:$DESKTOP_SESSION_PORT" /u:"$DESKTOP_SESSION_USER" /sec:tls \
      /cert:ignore /from-stdin:force /size:"$DESKTOP_SESSION_SIZE" /gfx:AVC420 -decorations \
      < "$session_dir/password" > "$artifacts/freerdp.log" 2>&1 &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true; wait "$client_pid" 2>/dev/null || true' EXIT
    for _ in {1..30}; do
      sleep 1
      magick import -window root "$artifacts/received-freerdp.png"
      if python /work/tools/check-desktop-frame.py "$artifacts/received-freerdp.png" "$DESKTOP_SESSION_SIZE" &&
        grep -q 'RDP EGFX frame ACK' "$artifacts/wayshare.log"; then
        echo 'FreeRDP desktop capture: PASS' | tee "$artifacts/result.txt"
        exit 0
      fi
      kill -0 "$client_pid" 2>/dev/null || break
    done
    echo 'FreeRDP desktop capture: FAIL' | tee "$artifacts/result.txt"
    exit 1
    ;;
  *) echo "Unknown session phase: $phase" >&2; exit 1 ;;
esac
