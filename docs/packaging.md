# Packaging

Notes for distro packagers.

## Build

```sh
cargo build --release --frozen
```

Builds need Rust 1.89 or newer, a C linker, `pkg-config`, and the EGL and GBM
development files (`mesa` and `libglvnd` on Arch). Default builds also need a C++
compiler. `Cargo.toml` sets the minimum Rust version. The pin in
`rust-toolchain.toml` is for contributors. `Cargo.lock` is committed.

Default builds include RDP and build OpenH264 from source through the
`openh264` crate. No system OpenH264 package is needed. NASM is optional on x86,
and the build omits assembly if it is unavailable. `--no-default-features` omits
RDP and OpenH264.

`libz-sys` uses system zlib when available and builds a bundled copy otherwise.
Install the [zlib development files](development.md#build-prerequisites) to use
the distro library.

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
| C++ runtime (`libstdc++` with GCC on Linux) | linked in default builds | OpenH264 software encoding for RDP |
| zlib (`libz.so.1`) | linked when system zlib is available at build time | Compression. RDP dependencies select this backend |
| libva and a driver that encodes H.264 | optional, loaded at runtime | RDP H.264 via VA-API. OpenH264 provides the software fallback |
| libva and a driver that encodes HEVC | optional, loaded at runtime | High Performance via VA-API |
| x265 (API build 217) | optional, loaded at runtime | High Performance without VA-API |
| fdk-aac | optional, loaded at runtime | High Performance audio |
| libpulse, plus a PulseAudio-compatible server such as `pipewire-pulse` | optional, loaded at runtime | High Performance audio |

Optional libraries are opened by name (`libva-drm.so.2`, `libx265.so`, `libfdk-aac.so.2`,
`libpulse.so.0`). On Nix, put them on the runtime library path.
