#!/bin/sh
# Installs the latest wayshare release into ~/.local/bin:
#   curl -fsSL https://raw.githubusercontent.com/wesleygrimes/wayshare/main/install.sh | sh
#
#   WAYSHARE_VERSION      a release tag such as v0.1.0 (default: latest)
#   WAYSHARE_INSTALL_DIR  default ~/.local/bin
set -eu

repo=wesleygrimes/wayshare

main() {
  [ "$(uname -s)" = Linux ] || fail "wayshare runs on Linux only"
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "no build for $(uname -m)" ;;
  esac
  for cmd in curl tar sha256sum; do
    command -v "$cmd" >/dev/null || fail "needs $cmd"
  done

  name=wayshare-$arch-unknown-linux-gnu
  case ${WAYSHARE_VERSION:-latest} in
    latest) url=https://github.com/$repo/releases/latest/download ;;
    *) url=https://github.com/$repo/releases/download/$WAYSHARE_VERSION ;;
  esac
  dir=${WAYSHARE_INSTALL_DIR:-$HOME/.local/bin}

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  cd "$tmp"
  echo "Downloading $name.tar.gz"
  curl -fsSL --proto '=https' -O "$url/$name.tar.gz" -O "$url/sha256sums.txt"
  grep " $name.tar.gz\$" sha256sums.txt | sha256sum -c --quiet - || fail "checksum mismatch"
  tar -xzf "$name.tar.gz"

  mkdir -p "$dir"
  install -m755 "$name/wayshare" "$dir/wayshare"
  echo "Installed $("$dir/wayshare" --version) to $dir/wayshare"

  case :$PATH: in
    *:"$dir":*) ;;
    *) echo "Add $dir to your PATH to run wayshare from a shell." ;;
  esac
  echo "Next, run: wayshare setup"
}

fail() {
  echo "error: $*" >&2
  exit 1
}

main "$@"
