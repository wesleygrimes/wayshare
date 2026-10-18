# Development

```sh
cargo test
```

Tests run on Linux and macOS. Capture, input and encoding only build on Linux.

## Build prerequisites

Rust 1.89 or newer ([rustup](https://rustup.rs) or your distro's package), a C linker,
`pkg-config`, and the EGL and GBM development files:

| Distro | Packages |
|---|---|
| Arch | `base-devel mesa libglvnd` |
| Debian, Ubuntu | `build-essential pkg-config libegl-dev libgbm-dev` |
| Fedora | `gcc pkgconf-pkg-config libglvnd-devel mesa-libgbm-devel` |

## Installing from source

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
| `mise run check:commits <base>` | check that commits in `<base>..HEAD` are Conventional Commits |
| `mise run build` | dev build; add `--release` for the optimized, locked build |
| `mise run audit` | cargo-deny: advisories, licenses, sources |
| `mise run changelog` | preview unreleased notes on stdout; writes nothing |
| `mise run release <version> [--dry-run] [--tag]` | prepare a release PR, or tag it; see [releasing.md](releasing.md) |
| `mise run gen-vaapi-sys` | regenerate the libva bindings (Linux) |

`mise run build` is now a dev build; pass `--release` for an optimized build.

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
