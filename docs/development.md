# Development

```sh
cargo test
```

Tests run on Linux and macOS. Capture, input and encoding only work on Linux.
Default builds include RDP; `cargo build --locked --no-default-features` omits it.

## Build prerequisites

Rust 1.89 or newer ([rustup](https://rustup.rs) or your distro's package), a C linker,
`pkg-config`, and the EGL and GBM development files:

| Distro | Packages |
|---|---|
| Arch | `base-devel mesa libglvnd` |
| Debian, Ubuntu | `build-essential pkg-config libegl-dev libgbm-dev` |
| Fedora | `gcc gcc-c++ pkgconf-pkg-config libglvnd-devel mesa-libgbm-devel` |

Default builds also need a C++ compiler for the OpenH264 software fallback.
`--no-default-features` omits RDP and OpenH264.

`libz-sys` uses system zlib when available and builds a bundled copy otherwise.
The optional development packages are `zlib` on Arch, `zlib1g-dev` on Debian and
Ubuntu, and `zlib-devel` on Fedora.

## Install from source

```sh
cargo install --locked --path .
wayshare setup
```

This puts `wayshare` in `~/.cargo/bin` and its unit in `~/.local/share/systemd/user`.

## mise

[mise](https://mise.jdx.dev) is optional, but CI runs these same tasks. It reads the
toolchain pin from `rust-toolchain.toml` and pins ShellCheck, actionlint, cargo-deny,
cocogitto, git-cliff and `bindgen-cli`. Run `mise run setup` once to install them. Setup lists any
missing Linux system packages but never uses sudo.

| Task | Does |
|---|---|
| `mise run setup` | install tools and Rust components, report missing system packages |
| `mise run fmt` | format with rustfmt |
| `mise run fmt:check` | check formatting |
| `mise run lint` | clippy (warnings fail), ShellCheck, actionlint |
| `mise run test` | `cargo test --locked` |
| `mise run bench:media` | benchmark HEVC packetization and SRTP encryption; no GPU needed |
| `mise run check` | `fmt:check`, `lint` and `test`; rewrites nothing |
| `mise run check:no-rdp` | clippy and tests with `--no-default-features` |
| `mise run check:commits <base>` | check that commits in `<base>..HEAD` are Conventional Commits |
| `mise run build` | dev build; add `--release` for the optimized, locked build |
| `mise run audit` | cargo-deny for default and all features: advisories, licenses, sources |
| `mise run changelog` | preview unreleased notes on stdout; writes nothing |
| `mise run release <version> [--dry-run] [--tag]` | prepare a release PR, or tag it; see [releasing.md](releasing.md) |
| `mise run gen-vaapi-sys` | regenerate the libva bindings (Linux) |

`mise run build` uses the dev profile. Pass `--release` for an optimized build.

`audit` downloads the advisory database, so it needs network. `gen-vaapi-sys` needs libva
headers and clang. Normal checks do not need a live Hyprland session.

`bench:media` uses the optimized build and reports median time per call for a 1308-byte
SRTP payload, a 64 KiB HEVC frame and a 4 MiB refresh. Compare runs on the same idle
machine. These timings exclude capture, encoding, network transit and viewer buffering;
they do not measure input-to-display latency. Live sessions log capture, encode and send
times under `HP stats` every five seconds.

On Linux, set `WAYSHARE_BENCH_OUTPUT` to also measure GPU change detection and VA-API
conversion. The benchmark captures that output once and reuses the frame:

```sh
WAYSHARE_BENCH_OUTPUT=HEADLESS-1 mise run bench:media
```

### Live exclusive-display test

This ignored test briefly disables other monitors, checks that workspaces move onto
the selected output, then restores the layout and workspace assignments. Run it only
from a live Hyprland desktop, with the output that must stay enabled named explicitly:

```sh
env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY WAYSHARE_TEST_OUTPUT=DP-1 \
  cargo test --locked live_exclusive_round_trip -- --ignored --nocapture
```

Cleanup also runs if a test assertion fails. A killed process cannot run cleanup;
use `hyprctl reload` to recover the configured monitor layout if needed.

## Build times

Measured 2026-10-16, median of three runs (two for release), `cargo` 1.98.1. Dev builds are
incremental; release builds are not.

- Mac: Apple M2 Pro, 12 cores, 32 GB, no compiler cache (`kache` disabled).
- Arch VM: AMD Ryzen 7 6800U, 8 vCPUs, 9.6 GB.

| Machine | Case | Command | Time |
|---|---|---|---|
| Mac | cold | `mise run build` | 8.8 s |
| Mac | no-op | `mise run build` | 0.2 s |
| Mac | edit `src/rfb/messages.rs` | `mise run build` | 1.1 s |
| Mac | cold release | `mise run build --release` | 14.5 s |
| VM | cold | `mise run build` | 18.9 s |
| VM | no-op | `mise run build` | 0.2 s |
| VM | edit `src/rfb/messages.rs` | `mise run build` | 1.7 s |
| VM | edit `src/server_linux.rs` | `mise run build` | 1.7 s |
| VM | cold release | `mise run build --release` | 30.7 s |

wayshare stays one package. Small edits already rebuild in under 2 s, and Linux-only
dependencies only compile on Linux, so a workspace split would save little and force many
internal items to be `pub`.

## Bindings

`src/encode/vaapi/sys.rs` is generated. Regenerate it with `tools/gen-vaapi-sys.sh` rather
than editing it.

## Style

Code is formatted with rustfmt (default config). Run `cargo fmt` before committing;
`cargo fmt --check` verifies. The generated `src/encode/vaapi/sys.rs` is excluded; regenerate
it rather than formatting it.

## RDP implementation

Default builds enable the `rdp` Cargo feature. See [Packaging](packaging.md#build)
for OpenH264 build requirements.

### Login and session identity

Credentials must pass before capture or input starts. Password verification runs
on blocking workers. Submitted passwords are not saved or logged. Validation
copies and decoded credentials are cleared after use, including cancellation.
This does not erase every temporary memory copy. Development ARD credentials
also work when configured, but RDP startup still requires a readable SRP record.

After activation and successful login, wayshare sends Save Session Info once,
with Logon Info Version 2, the accepted username, and an empty domain. A
process-wide counter assigns the 32-bit session ID. It resets on restart and
is not a reconnect cookie. The log line `RDP Save Session Info sent` contains
`username` and `session_id`. The
[iPhone rotation finding](development/rdp-ios-rotation.md) records why the client
needs that notification.

### Graphics and resize

The server selects the highest client EGFX version and preserves its flags.
H.264 tries VA-API Constrained Baseline on `/dev/dri/renderD128`, then OpenH264.
Coded dimensions round up to multiples of 16. The visible region crops the
padding. SPS, PPS, and an IDR start each H.264 stream. EGFX Planar uses top-down
RGB tiles.

The server keeps at most one unacknowledged frame. A two-second ACK or decode
stall resets the surface and encoder for a full refresh. A second failure or a
closed EGFX channel uses legacy graphics. The capture queue is bounded. For an
isolated loopback test, `WAYSHARE_TEST_RDP_FAIL_AFTER=20` injects one encoder
failure after 20 AVC420 frames. Network listeners ignore it.

Display Control advertises 16 monitors with area factors of 8192 by 8192, per
[MS-RDPEDISP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedisp/ea2de591-9203-42cd-9908-be7a55237d1c).
The server applies the primary monitor, or the first if none is marked, and logs
the others. The orientation field reports the client's rotation angle, not its
aspect ratio.

An EGFX resize deletes the known surface, sends ResetGraphics, recreates and
maps surface 0, and restarts capture and encoding on the same connection.
Legacy resizing uses deactivation and reactivation with a five-second deadline.
A repeated original size in Confirm Active does not undo the requested resize.
Pointer coordinates use the client extent, including odd initial desktop sizes.

Suppress Output pauses graphics but retains capture dirty state and control
traffic. Layouts apply during suppression. Resume sends a full fresh frame.
Refresh Rect requests a redraw without clearing suppression. Suppression does
not start a recovery probe or close timer. Legacy capture coalesces frames while
an update or socket backlog is pending.

### Local protocol patches

The handshake keeps negotiation flags `0x0b` with EGFX enabled, Graphics first
with DVC IDs starting at 1, and server core RDP 10.7. Server Monitor Layout PDUs
use zero `pduSource` and inclusive bounds, only when the client requests them.
These values do not enable NLA. The local
[server](../vendor/ironrdp-server/README.md),
[acceptor](../vendor/ironrdp-acceptor/README.md), and
[DVC](../vendor/ironrdp-dvc/README.md) notes describe the patches.

Connector 0.10 unconditionally links `picky` and `sspi` with smart-card support.
`deny.toml` ignores RUSTSEC-2023-0071 and RUSTSEC-2023-0089 for these unused RSA
and smart-card paths. Those exceptions do not establish safety for other uses.
No `WAYSHARE_RDP_*` environment variables are supported.
