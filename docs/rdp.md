# RDP

RDP shares your Hyprland monitor over TLS with the same username and password as
Apple Screen Sharing. Default builds enable it at `0.0.0.0:3389`, on all IPv4
interfaces. Builds with `--no-default-features` omit RDP.

Connection steps are in the [Windows App and mstsc](connect/windows-app.md) and
[FreeRDP](connect/freerdp.md) quickstarts. The [client matrix](clients.md) records
which clients the maintainer has tested.

## Settings

The `[rdp]` table in `~/.config/wayshare/config.toml` accepts these keys:

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` in default builds | Enables the TLS RDP listener |
| `listen` | `"0.0.0.0:3389"` | Listen address and port, independent of top-level Apple `listen` |
| `codec` | `"auto"` | Graphics selection. Values below |
| `identity_file` | Config directory's `rdp-identity.pem` | Absolute path to a PEM certificate and private key |

`--rdp`, `--no-rdp`, `--rdp-listen`, and `--rdp-identity-file` override the matching
settings. A LAN or VPN address limits the listener to that interface.
`[::]:3389` binds IPv6; IPv4 access depends on the host's socket policy.

RDP startup needs a readable SRP record and a usable TLS identity. A failure
leaves RDP off while Apple Screen Sharing keeps listening. Startup logs the bound
address and certificate's SHA-256 fingerprint. [Security](security.md) describes
certificate identity, shared credentials, and firewall ports.

## Login

The username is the exact, case-sensitive Apple-mode username from setup. The
domain is empty. Both protocols load the same `srp_file`, which defaults to
`~/.config/wayshare/srp-$USER` or `$XDG_CONFIG_HOME/wayshare/srp-$USER`.
Password changes take effect after a restart.

RDP checks the submitted password inside TLS against the stored SRP verifier.
It has no NLA, CredSSP, PAM, keyring, or smart-card login. RDP has no mDNS
advertisement. Clients connect by address.

## Graphics

| `rdp.codec` | Selection |
|---|---|
| `"auto"` | Prefers H.264/AVC420 over EGFX, the RDP Graphics Pipeline Extension |
| `"h264"` | Same preference and fallback rules as `"auto"` |
| `"remotefx"` | Forces legacy RemoteFX or bitmap updates |
| `"egfx-remotefx"` | Experimental RemoteFX over EGFX. Causes protocol errors in the iOS Windows App |

Clients that disable AVC420 use EGFX Planar. Clients without EGFX use legacy
RemoteFX or bitmap updates. Automatic selection excludes `"egfx-remotefx"`.
AVC444 is unsupported.

On Linux, H.264 tries VA-API on `/dev/dri/renderD128`, then OpenH264 in software.
If neither works, the connection uses legacy graphics. These encoders are
separate from Apple High Performance mode's HEVC encoder. Capture runs at up to
ten frames per second.

FreeRDP can request AVC420 with `/gfx:avc420`, Planar with
`/gfx:AVC420:off,AVC444:off,RFX:off,progressive:off`, or legacy RemoteFX with
`-gfx /rfx`.

## Resizing and input

Top-level `resize = true`, the default, fits the shared output to the client's
requested size and restores the original mode on disconnect. RDP and Apple
Screen Sharing share display ownership. One session owns mode changes at a time.
Frames scale to the client size if resizing is off, fails, or another session
owns the mode.

Display Control applies the primary monitor from a client layout, or the first
if none is primary. It ignores other monitors. Both dimensions must be between
200 and 8192 pixels. Display Control layout widths must be even.

Live resize and rotation work in Windows App on iPhone. The other
[tested RDP clients](clients.md) have verified connections only.

PC modifiers bypass the Mac Command and Option remapping. Keyboard scancodes,
Unicode text, pointer movement, five mouse buttons, and both wheel axes use the
shared input system. Disconnect releases held keys and buttons.
`[input] enabled = false` disables input for both protocols.

## Session limits

At most eight RDP connections can be active, including logins in progress.
New connections do not replace existing sessions, even from the same IP address.
Viewers share desktop focus. Exclusive display mode restores other monitors
after the last viewer leaves.

When the client suppresses output, graphics pause while control traffic continues.
Resize requests still apply. Resuming output sends a fresh frame. An ACK or decode
stall resets the graphics surface once before fallback to legacy graphics.
On Linux, a client that stops reading output has a five-second watchdog.

RDP has no clipboard or sound. `wayshare doctor` checks the ufw rule for the RDP
port. It does not connect to the RDP listener. Startup and session logs appear in `journalctl --user -u wayshare`.
`RUST_LOG=info,wayshare::rdp=debug` adds client capabilities, channel creation,
frame acknowledgements, and layout details. [Troubleshooting](troubleshooting.md#rdp)
has steps for connection and graphics failures.
