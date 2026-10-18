# Changelog

## 0.2.1 - 2026-10-18

### Fixes

- High Performance mode now imports all planes of GPU capture buffers and uses the HEVC limits reported by the driver. Tested on Intel Raptor Lake-P with iHD 26.2.4. Thanks @ChachizR. ([#48](https://github.com/wesleygrimes/wayshare/pull/48))

### Performance

- High Performance video sends each tile's packets as soon as they are ready and spends less CPU time packetizing and encrypting them. ([#49](https://github.com/wesleygrimes/wayshare/pull/49))

### Documentation

- The README puts installation and setup first and credits ChachizR for code. ([#46](https://github.com/wesleygrimes/wayshare/pull/46), [#50](https://github.com/wesleygrimes/wayshare/pull/50))

### Maintenance

- Fixes an intermittent macOS test failure when using a nonblocking fake Hyprland socket. ([#51](https://github.com/wesleygrimes/wayshare/pull/51))

**Full changelog**: [v0.2.0...v0.2.1](https://github.com/wesleygrimes/wayshare/compare/v0.2.0...v0.2.1)

## 0.2.0 - 2026-10-18

### Breaking changes

- Re-running the install command no longer restarts the service. Coming from 0.1.5 or older, run the install command, then `wayshare restart`. From now on, `wayshare upgrade` downloads and restarts in one step. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- `wayshare install-service` is gone. `wayshare setup` writes the service for you; units written by earlier versions keep working. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- Without `output` in the config, wayshare now shares your monitor when you have exactly one, and won't start when you have several. It used to serve a test pattern. `--test-pattern` brings that back for development. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))

### Features

- `wayshare setup` walks you through the password, the monitor and which network to listen on, such as Tailscale only. It starts the service, offers to add a ufw rule with sudo, and prints what to enter on the Mac. Running it again keeps your current choices as the defaults. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- `wayshare doctor` checks Hyprland, the monitor, the password, the service, the port, ufw and the High Performance encoder, and says how to fix each problem. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- `wayshare start`, `stop`, `restart`, `upgrade` and `uninstall` manage the service without `systemctl`. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- `wayshare set-password` uses your Linux user name unless you pass `--user`. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))
- With `exclusive = true`, wayshare turns off your other monitors while a viewer is connected and moves their workspaces onto the shared one, then puts everything back when the last viewer leaves. It's off by default. Thanks @bndnsmth. ([#38](https://github.com/wesleygrimes/wayshare/pull/38))

### Documentation

- A new security page covers what's encrypted, which ports to open, and limiting access to Tailscale or a VPN. ([#40](https://github.com/wesleygrimes/wayshare/pull/40), [#41](https://github.com/wesleygrimes/wayshare/pull/41))
- The README's setup section is now `wayshare setup`, and the guides that the new commands replace are shorter or gone. ([#41](https://github.com/wesleygrimes/wayshare/pull/41))

**Full changelog**: [v0.1.5...v0.2.0](https://github.com/wesleygrimes/wayshare/compare/v0.1.5...v0.2.0)

## 0.1.5 - 2026-10-17

### Fixes

- Capture now works on drivers that can't map GPU buffers for CPU reads, such as the NVIDIA proprietary driver. wayshare switches to shared memory capture on its own, and the error no longer says "Success". Thanks @ernestodisa for reporting it and pointing to the fix. ([#20](https://github.com/wesleygrimes/wayshare/pull/20))
- The screen no longer comes through scrambled on machines with two GPUs when the compositor uses wlr-screencopy. wayshare now allocates capture buffers on the compositor's main GPU. Thanks @andresark. ([#23](https://github.com/wesleygrimes/wayshare/pull/23))
- High Performance audio no longer starts before the viewer is ready or keeps sending after disconnect, and shutting it down no longer waits on a blocked read. Thanks @andresark. ([#24](https://github.com/wesleygrimes/wayshare/pull/24))
- High Performance mode now checks the encoder on the GPU that capture uses. On machines with two GPUs, if that GPU can't encode HEVC with VA-API, it falls back to x265 instead of failing to start. ([#35](https://github.com/wesleygrimes/wayshare/pull/35))
- The VA-API entrypoint query no longer overflows its buffer when a driver reports more than 32 entrypoints. ([#18](https://github.com/wesleygrimes/wayshare/pull/18))

### Maintenance

- The `input stats` log line now counts High Performance mouse events, to help track down mouse problems in that mode. Key contents are still never logged. Thanks @kyler505. ([#25](https://github.com/wesleygrimes/wayshare/pull/25))
- The README credits kyler505, ernestodisa and thebytorsnowdog as contributors, and andresark for code. ([#26](https://github.com/wesleygrimes/wayshare/pull/26), [#27](https://github.com/wesleygrimes/wayshare/pull/27), [#28](https://github.com/wesleygrimes/wayshare/pull/28), [#30](https://github.com/wesleygrimes/wayshare/pull/30))

**Full changelog**: [v0.1.3...v0.1.5](https://github.com/wesleygrimes/wayshare/compare/v0.1.3...v0.1.5)

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
