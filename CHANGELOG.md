# Changelog

## 0.1.1 - 2026-10-16

First public release.

### Fixes

- VA-API: creating surfaces no longer relies on undefined behavior.
- x265: a failed encoder restart or picture allocation now returns an error instead of crashing on the next frame.

### Maintenance

- Release binaries are stripped, so downloads are smaller.
- Contributor guide, issue templates, CI checks, and release tooling.

**Full changelog**: [v0.1.0...v0.1.1](https://github.com/wesleygrimes/wayshare/compare/v0.1.0...v0.1.1)

## 0.1.0 - 2026-10-15

First release.

- Share a Hyprland monitor with the Screen Sharing app built into macOS.
- High Performance mode: HEVC video through VA-API or x265, with sound.
- Keyboard, mouse and clipboard in both directions.
- Resizes the shared monitor to fit the Mac's window, and restores it afterwards.
- Shows up in Finder's sidebar on the Mac.
- Password login over SRP. Only a salted verifier is stored, never the password.
- `wayshare install-service` sets up the systemd user unit for installs without a package.
