#!/usr/bin/env bash
# Reports which Linux system packages for building wayshare are missing. Installs nothing.
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
  echo "System packages: none needed on $(uname -s) (only the portable code builds here)."
  exit 0
fi

# shellcheck disable=SC1091
. /etc/os-release
case " ${ID:-} ${ID_LIKE:-} " in
  *" arch "*)
    install="sudo pacman -S --needed"
    packages=(base-devel mesa libglvnd)
    installed() { pacman -Q "$1" >/dev/null 2>&1; }
    ;;
  *" debian "* | *" ubuntu "*)
    install="sudo apt install"
    packages=(build-essential pkg-config libegl-dev libgbm-dev)
    installed() { dpkg -s "$1" >/dev/null 2>&1; }
    ;;
  *" fedora "* | *" rhel "*)
    install="sudo dnf install"
    packages=(gcc pkgconf-pkg-config libglvnd-devel mesa-libgbm-devel)
    installed() { rpm -q "$1" >/dev/null 2>&1; }
    ;;
  *)
    echo "Unknown distro '${ID:-?}'. See docs/development.md for the packages to install."
    exit 0
    ;;
esac

missing=()
for pkg in "${packages[@]}"; do
  installed "$pkg" || missing+=("$pkg")
done

if ((${#missing[@]})); then
  echo "Missing system packages: ${missing[*]}"
  echo "Install them with: $install ${missing[*]}"
else
  echo "System packages: all present."
fi
