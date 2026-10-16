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

[mise](https://mise.jdx.dev) is optional. It reads the toolchain pin from
`rust-toolchain.toml` and installs `bindgen-cli`. Tasks:

| Task | Does |
|---|---|
| `mise run build` | release build |
| `mise run test` | `cargo test` |
| `mise run gen-vaapi-sys` | regenerate the libva bindings (Linux) |

## Bindings

`src/encode/vaapi/sys.rs` is generated. Regenerate it with `tools/gen-vaapi-sys.sh` rather
than editing it.

## Style

Code is formatted with rustfmt (default config). Run `cargo fmt` before committing;
`cargo fmt --check` verifies. The generated `src/encode/vaapi/sys.rs` is excluded; regenerate
it rather than formatting it.
