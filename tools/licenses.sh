#!/usr/bin/env bash
set -euo pipefail

builtin cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

check=false
if [[ ${1:-} == --check ]]; then
  check=true
  shift
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

status=0
cargo about -L warn --color never generate --locked --fail "$@" about.hbs \
  -o "$tmp/licenses" 2> "$tmp/stderr" || status=$?

# cargo-about 0.9.3 colors log levels even with --color never and exits 0
# after clarification failures. Reject those warnings before replacing notices.
sed $'s/\x1b\\[[0-9;]*m//g' "$tmp/stderr" > "$tmp/log"
cat "$tmp/log" >&2
if (( status != 0 )); then
  exit "$status"
fi
if grep -Eiq '(\[(WARN|ERROR)\]|^[[:space:]]*(warning|error)(:|\[|[[:space:]]|$))' "$tmp/log"; then
  echo "License generation reported a warning or error; THIRD-PARTY-LICENSES was not updated." >&2
  exit 1
fi

if "$check"; then
  if ! cmp -s "$tmp/licenses" THIRD-PARTY-LICENSES; then
    echo "THIRD-PARTY-LICENSES is out of date. Run mise run licenses and commit THIRD-PARTY-LICENSES." >&2
    exit 1
  fi
  echo "THIRD-PARTY-LICENSES is up to date."
else
  cp "$tmp/licenses" THIRD-PARTY-LICENSES
  echo "Wrote THIRD-PARTY-LICENSES. Commit it with the dependency changes."
fi
