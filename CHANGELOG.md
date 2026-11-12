# Changelog

## 0.4.0 - 2026-11-12

### Features

- With High Performance on, `wayshare setup` offers a ufw rule for UDP `hp.port` and `hp.port + 2` (5960 and 5962 by default). The rule names the interface when `listen` is bound to one and the LAN range otherwise, like the TCP 5900 rule. Without it, ufw drops the Mac's first audio RTCP packet and High Performance sound starts about 5 seconds late. `wayshare doctor` adds an `HP firewall` line that warns about missing UDP ports. ([#93](https://github.com/wesleygrimes/wayshare/pull/93))

### Fixes

- With `resize = true`, wayshare now switches a physical monitor only to sizes listed in its `availableModes`, at the closest listed refresh rate. For other sizes it keeps the current mode and the viewer scales the picture. An unlisted size could make a 1080p HDMI monitor lose signal and disconnect, which ended capture. Headless outputs still resize to any size. ([#92](https://github.com/wesleygrimes/wayshare/pull/92))
- When the compositor stops the Standard mode capture session, wayshare keeps showing the last frame and tries to open a new session about every 500 ms until the output returns, then sends a full refresh. Before, it logged an error on every frame and sent no new frames until the viewer disconnected. ([#92](https://github.com/wesleygrimes/wayshare/pull/92))
- Update `rustix` from 1.1.5 to 1.1.6 to address RUSTSEC-2026-0486, unsound handling of zero-length buffers in its extended-attribute functions. wayshare gets `rustix` through the Wayland client libraries.

### Upgrade notes

No configuration migration is required. If you use ufw with High Performance, rerun `wayshare setup` to add the UDP rule, or see [Ports](https://github.com/wesleygrimes/wayshare/blob/v0.4.0/docs/security.md#ports) to add it by hand. HDMI and DisplayPort dummy plugs count as physical monitors, so they now switch only to their listed modes. Use a headless output if you need arbitrary sizes.

**Full changelog**: [v0.3.2...v0.4.0](https://github.com/wesleygrimes/wayshare/compare/v0.3.2...v0.4.0)

## 0.3.2 - 2026-11-07

### Fixes

- High Performance now requires authenticated, parseable video RTCP feedback to keep streaming. Non-RTCP packets, authentication failures, and malformed RTCP no longer postpone fallback to Standard mode. Timeouts remain 15 seconds for initial feedback and 10 seconds without further valid feedback.
- Limit feedback processing to 32 UDP packets per loop pass so continuous incoming traffic leaves time for capture, timeout, and stop checks.

### Diagnostics and documentation

- Log the local media socket address and viewer endpoint. Add cumulative feedback and receive-error counters to `HP stats` and timeout errors. Authentication and parse failures warn once per HP stream.
- Explain how logs distinguish selected Standard mode, missing HP negotiation, setup failures, and feedback loss. Clarify UDP traffic directions, default ports 5960 and 5962, and stateful firewall rules. Doctor does not test this UDP path.

### Upgrade notes

No configuration migration is required. If High Performance falls back, use the connection logs and [troubleshooting guide](https://github.com/wesleygrimes/wayshare/blob/v0.3.2/docs/troubleshooting.md#high-performance-is-unavailable-or-falls-back) to check negotiation and valid feedback. A feedback timeout alone does not prove a firewall problem.

Thanks to [Matt](https://x.com/mbriggs_dev/status/2118891688001229109) for the configuration report that informed the network guidance.

**Full changelog**: [v0.3.1...v0.3.2](https://github.com/wesleygrimes/wayshare/compare/v0.3.1...v0.3.2)

## 0.3.1 - 2026-11-07

### Fixes

- Report the session's current display size and known scale to Mac Screen Sharing when a display configuration request cannot be applied or decoded. This addresses offset High Performance mouse clicks after a refused or failed resize. Successful changes still report the applied geometry. ([#83](https://github.com/wesleygrimes/wayshare/pull/83), addressing [#72](https://github.com/wesleygrimes/wayshare/issues/72))
- Update `libz-sys` from 1.1.30 to 1.1.31 to address RUSTSEC-2026-0418, an environment-variable leak in its build script. Update the bundled license notice to match. ([#84](https://github.com/wesleygrimes/wayshare/pull/84))

### Maintenance

- Update the CI mise action from 5.2.1 to 5.2.4. ([#81](https://github.com/wesleygrimes/wayshare/pull/81))
- Run compiling mise tasks through mr-boxington 1.29.2 and use its action in CI. Pin Rust 1.98 with rustfmt and clippy in mise. The release workflow continues to build with plain Cargo. ([#85](https://github.com/wesleygrimes/wayshare/pull/85))
- Accept advisory RUSTSEC-2026-0441 for 0.3.1 because `async-dnssd` depends on unmaintained `pin-utils`.

### Upgrade notes

No configuration migration is required. Contributors who use mise should run `mise run setup` to install the pinned build tools before running the updated tasks. Direct Cargo builds remain available.

Thanks to [Wes Grimes](https://github.com/wesleygrimes) for the display fix, dependency fix, and build changes, and to Dependabot for the CI dependency update.

**Full changelog**: [v0.3.0...v0.3.1](https://github.com/wesleygrimes/wayshare/compare/v0.3.0...v0.3.1)

## 0.3.0 - 2026-10-28

wayshare now works with RDP clients as well as Mac Screen Sharing.

### Upgrade notes

- RDP is on by default. It listens on port 3389 at the Apple listen address, so `0.0.0.0:3389` on a fresh install. Set `[rdp] listen` to use another address.
- To turn RDP off, set `[rdp] enabled = false` in the config or start wayshare with `--no-rdp`.
- If you use ufw, allow TCP 3389 or rerun setup so other machines can reach RDP.

### Features

- **rdp:** add an RDP server built on IronRDP. It shares the desktop with keyboard and pointer input, and resizes the desktop when the client window changes size or an iPhone rotates. Closes [#55](https://github.com/wesleygrimes/wayshare/issues/55). ([#74](https://github.com/wesleygrimes/wayshare/pull/74))
- **rdp:** RDP logs in over TLS with the same username and password as Apple mode. ([#74](https://github.com/wesleygrimes/wayshare/pull/74))
- **rdp:** tested with Windows App on iPhone, iPad and Mac, Windows Remote Desktop Connection (mstsc), and FreeRDP. ([#75](https://github.com/wesleygrimes/wayshare/pull/75))
- **doctor:** warn when RDP listens on a network address and ufw has no rule for its port. ([#75](https://github.com/wesleygrimes/wayshare/pull/75), [#76](https://github.com/wesleygrimes/wayshare/pull/76))

### Fixes

- **hp:** a VA-API bitrate overshoot no longer ends a High Performance stream. wayshare keeps the frame and logs one warning. ([#76](https://github.com/wesleygrimes/wayshare/pull/76))
- setup and doctor recognize more kinds of ufw rules, such as port ranges, `limit`, app profiles and subnet rules. A deny that covers all connections now hides a later allow. ([#76](https://github.com/wesleygrimes/wayshare/pull/76))
- **build:** `tools/check-system-deps.sh` now lists `gcc-c++` on Fedora, which OpenH264 needs.
- **build:** the source tree and release archives include THIRD-PARTY-LICENSES with the licenses of the bundled crates. ([#78](https://github.com/wesleygrimes/wayshare/pull/78))

### Documentation

- reorganize the docs for both protocols, with a "Connect from" table in the README and a quickstart for each client under docs/connect/ ([#75](https://github.com/wesleygrimes/wayshare/pull/75))

**Full changelog**: [v0.2.4...v0.3.0](https://github.com/wesleygrimes/wayshare/compare/v0.2.4...v0.3.0)

## 0.2.4 - 2026-10-25

### Fixes

- **capture:** align udmabuf allocations to the kernel page size, which fixes dropped connections on ARM64 kernels with 16 KiB pages ([#68](https://github.com/wesleygrimes/wayshare/pull/68))

### Documentation

- add Omarchy package install to readme ([#67](https://github.com/wesleygrimes/wayshare/pull/67))
- add 3j27 as a contributor for code, bug, platform, and test ([#69](https://github.com/wesleygrimes/wayshare/pull/69))

Thanks to Wes Grimes, 3j27.

**Full changelog**: [v0.2.3...v0.2.4](https://github.com/wesleygrimes/wayshare/compare/v0.2.3...v0.2.4)

## 0.2.3 - 2026-10-23

### Fixes

- Updates ctutils to 0.4.4 for RUSTSEC-2026-0385, which could leak secret values through timing. wayshare uses it through its hashing libraries. ([#65](https://github.com/wesleygrimes/wayshare/pull/65))

### Maintenance

- Updates libc, mdns-sd and plist to their latest patch releases. ([#64](https://github.com/wesleygrimes/wayshare/pull/64))

**Full changelog**: [v0.2.2...v0.2.3](https://github.com/wesleygrimes/wayshare/compare/v0.2.2...v0.2.3)

## 0.2.2 - 2026-10-22

### Fixes

- High Performance mode now updates the viewer's display size when the shared output changes size outside wayshare. ([#54](https://github.com/wesleygrimes/wayshare/pull/54))
- VA-API encoding now converts through video processing when the driver exports an implicit surface layout. ([#59](https://github.com/wesleygrimes/wayshare/pull/59))

### Maintenance

- New and reopened issues are assigned to the maintainer automatically. ([#61](https://github.com/wesleygrimes/wayshare/pull/61))

**Full changelog**: [v0.2.1...v0.2.2](https://github.com/wesleygrimes/wayshare/compare/v0.2.1...v0.2.2)

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
