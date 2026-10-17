# Changelog

## 0.1.4 - 2026-10-17

### Fixes

- Capture now works on drivers that can't map GPU buffers for CPU reads, such as the NVIDIA proprietary driver. wayshare switches to shared memory capture on its own, and the error no longer says "Success". Thanks @ernestodisa for reporting it and pointing to the fix. ([#20](https://github.com/wesleygrimes/wayshare/pull/20))
- The screen no longer comes through scrambled on machines with two GPUs when the compositor uses wlr-screencopy. wayshare now allocates capture buffers on the compositor's main GPU. Thanks @andresark. ([#23](https://github.com/wesleygrimes/wayshare/pull/23))
- High Performance audio no longer starts before the viewer is ready or keeps sending after disconnect, and shutting it down no longer waits on a blocked read. Thanks @andresark. ([#24](https://github.com/wesleygrimes/wayshare/pull/24))
- The VA-API entrypoint query no longer overflows its buffer when a driver reports more than 32 entrypoints. ([#18](https://github.com/wesleygrimes/wayshare/pull/18))

### Maintenance

- The `input stats` log line now counts High Performance mouse events, to help track down mouse problems in that mode. Key contents are still never logged. Thanks @kyler505. ([#25](https://github.com/wesleygrimes/wayshare/pull/25))
- The README credits kyler505, ernestodisa and thebytorsnowdog as contributors, and andresark for code. ([#26](https://github.com/wesleygrimes/wayshare/pull/26), [#27](https://github.com/wesleygrimes/wayshare/pull/27), [#28](https://github.com/wesleygrimes/wayshare/pull/28), [#30](https://github.com/wesleygrimes/wayshare/pull/30))

**Full changelog**: [v0.1.3...v0.1.4](https://github.com/wesleygrimes/wayshare/compare/v0.1.3...v0.1.4)

## 0.1.3 - 2026-10-17

### Fixes

- The screen no longer stays black in virtual machines with a virtio-gpu display, such as UTM on a Mac. wayshare now captures through shared memory there. Thanks @kitknox. ([#12](https://github.com/wesleygrimes/wayshare/pull/12))

### Maintenance

- The README credits Kit Knox as a contributor. ([#14](https://github.com/wesleygrimes/wayshare/pull/14))
- When a PR title doesn't follow the required format, CI now says how to fix it. ([#15](https://github.com/wesleygrimes/wayshare/pull/15))

**Full changelog**: [v0.1.2...v0.1.3](https://github.com/wesleygrimes/wayshare/compare/v0.1.2...v0.1.3)

## 0.1.2 - 2026-10-17

### Fixes

- Capture no longer shows scrambled frames on GPUs that hand out tiled buffers. Thanks @andresark. ([#11](https://github.com/wesleygrimes/wayshare/pull/11))

### Documentation

- The README has a demo video and a real screenshot, and leads with the Screen Sharing app for connecting. ([#4](https://github.com/wesleygrimes/wayshare/pull/4), [#6](https://github.com/wesleygrimes/wayshare/pull/6), [#7](https://github.com/wesleygrimes/wayshare/pull/7), [#8](https://github.com/wesleygrimes/wayshare/pull/8))
- The README notes that the AUR and `omarchy pkg` installs are coming soon. ([#5](https://github.com/wesleygrimes/wayshare/pull/5))
- Pull requests are open to everyone, and the README credits contributors. ([#10](https://github.com/wesleygrimes/wayshare/pull/10))

**Full changelog**: [v0.1.1...v0.1.2](https://github.com/wesleygrimes/wayshare/compare/v0.1.1...v0.1.2)

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
