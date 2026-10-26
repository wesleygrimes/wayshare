# Configuration

wayshare reads `~/.config/wayshare/config.toml` (or `$XDG_CONFIG_HOME/wayshare/config.toml`).
Every key is optional. Command-line flags override the file; `wayshare --help` lists them.

A full example with every key lives in
[`contrib/config.example.toml`](../contrib/config.example.toml), and packages install it to
`/usr/share/doc/wayshare/`.

After changing anything, run `wayshare restart`.

## The basics

```toml
output = "DP-1"          # the monitor to share; `hyprctl monitors` lists them
name = "studio"          # how the machine appears on the Mac; default: the host name
listen = "0.0.0.0:5900"  # all networks; use one address to listen only there
resize = true            # match the Mac's window size, restore the monitor afterwards
```

`wayshare setup` picks `output` and `listen` for you. To listen only over Tailscale, see
[Security](security.md#only-accept-connections-over-tailscale).

With `resize` on, wayshare switches the shared monitor to the mode the Mac asks for while
you're connected, and puts it back when you disconnect.

## Exclusive display

To use only the shared monitor while connected, add this at the top level:

```toml
output = "DP-1"
exclusive = true
```

After the first viewer authenticates, wayshare saves the monitor layout, workspace
assignments and focus, then disables the other enabled monitors. Hyprland moves their
workspaces onto the shared monitor. After the last viewer disconnects, wayshare re-enables
the original monitors and moves surviving workspaces back. Already-disabled monitors stay
disabled. Restore rules are also sent for unplugged monitors so they are enabled again
when replugged, but wayshare does not wait for absent monitors or move workspaces onto
them. Closed workspaces are skipped; monitors added during sharing are left alone.

wayshare uses exact-output-name monitor rules. Existing rules for the same output name
retain settings that wayshare does not change, including the configured VRR mode.
Settings inherited from `desc:` or fallback rules can instead be shadowed by these runtime
rules: bit depth, color management, mirroring, reserved areas and per-monitor VRR may fall
back to defaults. Use explicit output-name rules to avoid this limitation, or run
`hyprctl reload` afterwards to recover the configured rules.

This is off by default, requires `output`, and works in Standard and High Performance mode,
independently of `resize`. Session errors and graceful server shutdown also restore the
desktop. A process abort or SIGKILL cannot run cleanup: `hyprctl reload` recovers the
configured monitor layout, but not the saved workspace assignments.

The flags `--exclusive` and `--no-exclusive` override the setting.

## Login

`wayshare set-password` stores a salted verifier, never the password, in `srp-$USER` next to
the config file. Run it again to change the password. `--user NAME` sets a different login
name and writes `srp-NAME`.

wayshare looks for `srp-$USER` unless told otherwise, so a login name other than your Linux
user name needs `srp_file` too:

```toml
srp_file = "/home/alice/.config/wayshare/srp-studio"
```

Paths in the config must be absolute; `~` isn't expanded.

## Keyboard

```toml
[input]
enabled = true
command_key = "super"    # super | ctrl | alt | altgr
option_key = "alt"
layout = "us"
variant = ""
```

`command_key` decides what the Mac's Command key sends. `super` suits Omarchy, whose
shortcuts live on Super.

## High Performance

```toml
[hp]
enabled = true
port = 5960              # video; audio uses port + 2
encoder = "auto"         # auto | vaapi | x265
chroma = "420"           # "444" needs encoder = "x265"
audio = true
# bitrate = 40000000
```

See [High Performance mode](high-performance.md).

## Finder sidebar

```toml
[mdns]
enabled = true
# device_model = "MacPro7,1"   # picks the icon Finder shows
```

## RDP

Default builds include RDP with TLS, local Linux password login through PAM, and input.
The runtime listener stays disabled, with `127.0.0.1:3389` as its default address.

```toml
[rdp]
enabled = false
listen = "127.0.0.1:3389"
# allowed_user = "alice"  # defaults to the account running wayshare
# identity_file = "/home/alice/.config/wayshare/rdp-identity.pem"
```

An explicit network address is allowed. Run as the desktop owner; on Arch a non-root
process cannot verify another user's password. See [RDP](rdp.md) for PAM packaging,
username policy, certificate verification and limitations. `--no-default-features`
omits RDP from the build.
