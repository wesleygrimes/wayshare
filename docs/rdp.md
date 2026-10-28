# RDP

RDP streams the shared monitor over TLS with H.264/AVC420, Planar, legacy RemoteFX
or bitmap updates. It uses the same username and password as Apple Screen Sharing.
Default builds include RDP, but its listener is disabled until enabled in config.
Build with `--no-default-features` to omit it.

## Enable the listener

```toml
[rdp]
enabled = true
listen = "127.0.0.1:3389"
codec = "auto"
# identity_file = "/home/alice/.config/wayshare/rdp-identity.pem"
```

`--rdp`, `--no-rdp`, `--rdp-listen` and `--rdp-identity-file` override these settings.
For remote access, choose a LAN or Tailscale address, or use `0.0.0.0:3389`.
`[::]:3389` binds IPv6; IPv4 access depends on the host's socket policy.
Startup requires Apple mode credentials. Allow the chosen TCP port through your
firewall. RDP has no discovery advertisement.

## Login and TLS

Use the exact, case-sensitive Apple mode username and leave the domain empty.
The username need not be a Linux account. Both protocols load the same `srp_file`,
which defaults to `$XDG_CONFIG_HOME/wayshare/srp-$USER` or
`~/.config/wayshare/srp-$USER`. It stores a salt and verifier, not a password.
RDP verifies the submitted password against that record. Restart after changing it.
The development ARD credentials also work when configured. There is no PAM,
keyring, NLA, CredSSP or smart-card login in this adapter.

Credentials must pass before capture updates or input start. Failed attempts wait
at least two seconds. At most eight connections are admitted, with a 30-second
login deadline. Password verification runs on blocking workers. Submitted passwords
are not saved or logged; owned validation copies and decoded credentials are cleared
after use, including cancellation. This does not erase every temporary memory copy.

After activation and successful login, the server sends Save Session Info once,
using Logon Info Version 2, the authenticated username and an empty domain.
A process-wide counter assigns the 32-bit session ID. It distinguishes logons
within that server process and resets on restart; it is not a reconnect cookie.
The log is `RDP Save Session Info sent` with `username` and `session_id`.
This notification enables live rotation in the iOS Windows App. See the
[rotation finding](rdp-ios-rotation.md).

On first use, wayshare creates a self-signed localhost ECDSA certificate and key in
one PEM file with mode 0600. It reuses that identity. The default path is
`$XDG_CONFIG_HOME/wayshare/rdp-identity.pem`, or
`~/.config/wayshare/rdp-identity.pem`. `identity_file` must be absolute.
Invalid existing files cause an error and are not replaced.

Startup logs the bound address and the certificate's SHA-256 fingerprint as
colon-separated hex bytes. Compare it with the client's SHA-256 certificate
details before sending a password. A SHA-1 thumbprint cannot be compared with it.
For remote access, supply a trusted certificate and PKCS#8 key with the right
hostnames, or check the fingerprint through another trusted path.

```sh
openssl x509 -in /absolute/path/rdp-identity.pem -noout -fingerprint -sha256
xfreerdp3 /v:127.0.0.1:3389 /sec:tls /u:alice /from-stdin:force
```

## Graphics

`rdp.codec` accepts `"auto"`, `"h264"`, `"remotefx"` and `"egfx-remotefx"`.
`"auto"` and `"h264"` prefer H.264/AVC420 over the Graphics Pipeline Extension,
MS-RDPEGFX. Clients that disable AVC420 use EGFX Planar with top-down RGB tiles.
Clients without EGFX use legacy RemoteFX or compressed bitmap updates.
`"remotefx"` forces that legacy path. `"egfx-remotefx"` selects experimental
CAVIDEO; it causes protocol errors in the iOS Windows App and is excluded from
automatic selection.

The server selects the highest client EGFX version and preserves its flags.
On Linux, H.264 tries VA-API Constrained Baseline on `/dev/dri/renderD128`, then
OpenH264 in software. If neither works, the connection uses legacy graphics.
These encoders are separate from Apple high-performance mode's HEVC encoder.
Coded dimensions round up to multiples of 16; the visible region crops the padding.
SPS, PPS and an IDR start each H.264 stream.

The server keeps at most one unacknowledged frame. A two-second ACK or decode
stall resets the surface and encoder for a full refresh. A second failure or a
closed EGFX channel uses legacy graphics for that connection. The capture queue
is bounded and runs at up to ten frames per second.

FreeRDP can request AVC420 with `/gfx:avc420`, Planar with
`/gfx:AVC420:off,AVC444:off,RFX:off,progressive:off`, or legacy RemoteFX with
`-gfx /rfx`. For an isolated loopback test,
`WAYSHARE_TEST_RDP_FAIL_AFTER=20` injects one encoder failure after 20 AVC420
frames. Network listeners ignore it.

## Resizing and input

Enable top-level `resize = true` to fit the shared output to the client's pixel
size. RDP shares display ownership with RFB and restores the original mode on
disconnect. Only one session can own mode changes. Frames scale to the requested
size when resizing is disabled, fails, or another session owns the mode.
Pointer coordinates use that same extent, including odd initial desktop sizes.

Display Control advertises 16 monitors with area factors of 8192 by 8192. wayshare
applies the primary monitor, or the first if none is marked, and logs the others.
Layout widths must be even; both dimensions must be between 200 and 8192 pixels,
per [MS-RDPEDISP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedisp/ea2de591-9203-42cd-9908-be7a55237d1c).
The orientation log reports the client's rotation field, not the aspect ratio.

An EGFX size change deletes the known surface, sends ResetGraphics, recreates and
maps surface 0, and restarts capture and the encoder on the same connection.
Legacy resizing uses deactivation and reactivation with a five-second deadline.
A repeated original size in Confirm Active does not undo the requested resize.

Suppress Output pauses graphics while control traffic continues and capture
retains dirty state. Layouts apply during suppression; resume sends a full fresh
frame. Refresh Rect requests a redraw without clearing suppression. Suppression
does not start a recovery probe or close timer. The five-second output watchdog
closes clients that stop reading on Linux. New connections never replace existing
sessions, including connections from the same IP. Authenticated viewers may connect
concurrently, as in Apple RFB mode. Each connection has its own session ID. Viewers
share desktop focus, and the first session to change the output mode owns it until
disconnect. Exclusive display mode restores other monitors after the last viewer
leaves. Legacy capture coalesces frames while an update or socket backlog is pending.

Keyboard scancodes and Unicode input use the shared input sink. PC modifiers
bypass Mac Command/Option remapping. Absolute and relative pointer movement, five
buttons and both wheel axes use the shared pointer engine. Disconnect releases
held keys and buttons. `[input] enabled = false` disables input for both protocols.
Concurrent RFB and RDP viewers share desktop focus.

## Diagnostics and dependencies

Use `RUST_LOG=info,wayshare::rdp=debug` for client capabilities, DVC creation,
EGFX frame ACKs and layouts. Info logs include Display Control caps, Save Session
Info, suppression, surface resets and session summaries. No `WAYSHARE_RDP_*`
environment variables are needed or supported.

The handshake keeps negotiation flags `0x0b` when EGFX is enabled, Graphics first
with DVC IDs starting at 1, and server core RDP 10.7. Server Monitor Layout PDUs
use zero `pduSource` and inclusive bounds, only when the client requests them.
These values do not enable NLA. The local
[server](../vendor/ironrdp-server/README.md),
[acceptor](../vendor/ironrdp-acceptor/README.md) and
[DVC](../vendor/ironrdp-dvc/README.md) patches describe the changes.

OpenH264 builds from source under its BSD license. Connector 0.10 unconditionally
links `picky` and `sspi` with smart-card support. `deny.toml` ignores
RUSTSEC-2023-0071 and RUSTSEC-2023-0089 for these unused RSA and smart-card paths.
Those exceptions do not establish safety for other uses.

Clipboard, sound and AVC444 are unsupported. The iPhone rotation result does not
establish compatibility with every Microsoft client. `mise run check` covers the
default build and `mise run check:no-rdp` covers the build without RDP.
