#!/usr/bin/env bash
# Run on the Ubuntu runner after the wayshare RDP port is ready.
set -euo pipefail
: "${DESKTOP_SESSION_AGENT:?}" "${DESKTOP_SESSION_ENDPOINT:?}" "${DESKTOP_SESSION_ARTIFACTS:?}"
: "${DESKTOP_SESSION_HOST:?}" "${DESKTOP_SESSION_PORT:?}" "${DESKTOP_SESSION_USER:?}"
: "${DESKTOP_SESSION_PASSWORD_FILE:?}" "${DESKTOP_SESSION_SIZE:?}" "${DESKTOP_SESSION_CONTAINER:?}"

agent=$DESKTOP_SESSION_AGENT
endpoint=$DESKTOP_SESSION_ENDPOINT
artifacts=$DESKTOP_SESSION_ARTIFACTS
XDG_STATE_HOME="$(dirname "$endpoint")/state"
export XDG_STATE_HOME
width=${DESKTOP_SESSION_SIZE%x*}
height=${DESKTOP_SESSION_SIZE#*x}
result=FAIL
trap 'printf "ironrdp-agent desktop capture: %s\n" "$result" > "$artifacts/ironrdp-agent-result.txt"' EXIT

"$agent" --endpoint "$endpoint" daemon start --skip-certificate-check > "$artifacts/ironrdp-agent-start.log" 2>&1
# Passwords travel through IPC; neither argv nor artifacts contain the secret.
RDP_PASSWORD=$(cat "$DESKTOP_SESSION_PASSWORD_FILE") "$agent" --endpoint "$endpoint" connect \
  --no-auto-start --no-prompt --server "$DESKTOP_SESSION_HOST:$DESKTOP_SESSION_PORT" \
  --username "$DESKTOP_SESSION_USER" --domain '' \
  --prop ironrdp_tls:i:1 --prop enablecredsspsupport:i:0 \
  --prop "desktopwidth:i:$width" --prop "desktopheight:i:$height" \
  > "$artifacts/ironrdp-agent-connect.log" 2>&1

for _ in {1..30}; do
  "$agent" --endpoint "$endpoint" status > "$artifacts/ironrdp-agent-status.log" 2>&1
  if grep -q '^state: Connected' "$artifacts/ironrdp-agent-status.log"; then
    if "$agent" --endpoint "$endpoint" screenshot "$artifacts/received-ironrdp-agent.png" \
      > "$artifacts/ironrdp-agent-screenshot.log" 2>&1 &&
      docker exec "$DESKTOP_SESSION_CONTAINER" python /work/tools/check-desktop-frame.py \
        /artifacts/received-ironrdp-agent.png "$DESKTOP_SESSION_SIZE"; then
      result=PASS
      exit 0
    fi
  fi
  sleep 1
done
"$agent" --endpoint "$endpoint" query-logs --last 100 > "$artifacts/ironrdp-agent-session.log" 2>&1 || true
exit 1
