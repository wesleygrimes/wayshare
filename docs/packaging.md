# Packaging

Notes for distro packagers.

## Build

```sh
cargo build --release --frozen
```

Build dependencies: Rust 1.89 or newer, a C linker, `pkg-config`, and the EGL and GBM
development files (`mesa` and `libglvnd` on Arch). The minimum Rust is 1.89 (`rust-version` in `Cargo.toml`); the pin in
`rust-toolchain.toml` is for contributors and can be ignored. `Cargo.lock` is committed.

## Install

```sh
install -Dm755 target/release/wayshare "$pkgdir/usr/bin/wayshare"
sed 's|@bindir@|/usr/bin|' contrib/wayshare.service.in > wayshare.service
install -Dm644 wayshare.service "$pkgdir/usr/lib/systemd/user/wayshare.service"
install -Dm644 contrib/config.example.toml "$pkgdir/usr/share/doc/wayshare/config.example.toml"
```

Don't enable the unit globally. Each user runs `wayshare setup`, which sets a password,
writes their config and enables the unit.

## Dependencies

| Dependency | Kind | For |
|---|---|---|
| Hyprland >= 0.56 | runtime | everything |
| libEGL, libgbm (Mesa) | linked | capture |
| libva and a driver that encodes HEVC | optional, loaded at runtime | High Performance via VA-API |
| x265 (API build 217) | optional, loaded at runtime | High Performance without VA-API |
| fdk-aac | optional, loaded at runtime | High Performance audio |
| libpulse, plus a PulseAudio-compatible server such as `pipewire-pulse` | optional, loaded at runtime | High Performance audio |

Optional libraries are opened by name (`libva-drm.so.2`, `libx265.so`, `libfdk-aac.so.2`,
`libpulse.so.0`). On Nix, put them on the runtime library path.
