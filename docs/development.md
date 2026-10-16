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
wayshare install-service
```

This puts `wayshare` in `~/.cargo/bin` and its unit in `~/.local/share/systemd/user`. To
upgrade, run both again, then `systemctl --user restart wayshare`.

## mise

[mise](https://mise.jdx.dev) is optional, but CI runs these same tasks. It reads the
toolchain pin from `rust-toolchain.toml` and pins ShellCheck, actionlint, cargo-deny,
cocogitto and `bindgen-cli`. Run `mise run setup` once to install them. Setup lists any
missing Linux system packages but never uses sudo.

| Task | Does |
|---|---|
| `mise run setup` | install tools and Rust components, report missing system packages |
| `mise run fmt` | format with rustfmt |
| `mise run fmt:check` | check formatting |
| `mise run lint` | clippy (warnings fail), ShellCheck, actionlint |
| `mise run test` | `cargo test --locked` |
| `mise run check` | `fmt:check`, `lint` and `test`; rewrites nothing |
| `mise run check:commits <base>` | check that commits in `<base>..HEAD` are Conventional Commits |
| `mise run build` | dev build; add `--release` for the optimized, locked build |
| `mise run audit` | cargo-deny: advisories, licenses, sources |
| `mise run gen-vaapi-sys` | regenerate the libva bindings (Linux) |

`mise run build` is now a dev build; pass `--release` for an optimized build.

`audit` downloads the advisory database, so it needs network. `gen-vaapi-sys` needs libva
headers and clang. Nothing here needs a live Hyprland session.

## Bindings

`src/encode/vaapi/sys.rs` is generated. Regenerate it with `tools/gen-vaapi-sys.sh` rather
than editing it.

## Style

Code is formatted with rustfmt (default config). Run `cargo fmt` before committing;
`cargo fmt --check` verifies. The generated `src/encode/vaapi/sys.rs` is excluded; regenerate
it rather than formatting it.
