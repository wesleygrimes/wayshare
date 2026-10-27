# RDP

RDP streams the shared monitor over TLS with H.264/AVC420, RemoteFX or compressed bitmap updates.
It uses the same username and password as Apple Screen Sharing, including high
performance mode. RDP is included in the default Cargo features. Its runtime listener
is disabled by default, with `127.0.0.1:3389` as the default address.
Build with `--no-default-features` to omit RDP.

## Enable the listener

```toml
[rdp]
enabled = true
listen = "127.0.0.1:3389"
codec = "auto"
# identity_file = "/home/alice/.config/wayshare/rdp-identity.pem"
```

`--rdp`, `--no-rdp`, `--rdp-listen` and `--rdp-identity-file` override these settings.
A build without RDP rejects an enabled listener. To accept connections from another
device, set `listen = "0.0.0.0:3389"`, or choose one LAN or Tailscale address.
`[::]:3389` binds IPv6; IPv4 access depends on the host's socket policy.
Startup requires Apple mode credentials before opening the RDP listener.
Allow the chosen TCP port through your firewall and verify the certificate before
sending a password. RDP has no discovery advertisement. Port 3389 is the standard port.

Enabling RDP automatically alongside RFB would require changing `rdp.enabled` and
choosing a bind address. That changes which network listeners start, so it remains an
explicit configuration choice.

## Video codec

`rdp.codec` accepts `"auto"`, `"h264"` and `"remotefx"`. The default, `"auto"`, prefers
H.264/AVC420 over the Graphics Pipeline Extension, MS-RDPEGFX. `"h264"` has the same
fallback rules. `"remotefx"` disables EGFX and uses the client's legacy RemoteFX
capability. Clients without RemoteFX receive compressed bitmap updates.

The server confirms the highest client-advertised EGFX version. It uses AVC420 only
when that version permits it. Version 8.1 requires `AVC420_ENABLED`. Clients without
AVC420 support use RemoteFX over EGFX, with codec ID `CAVIDEO`. This does not require
legacy RemoteFX capabilities. Clients without EGFX use legacy RemoteFX or bitmap
updates. The server preserves the client's capability flags. When AVC is available,
it sends AVC420 only, even if the selected version also supports AVC444.

On Linux, H.264 first tries VA-API Constrained Baseline on `/dev/dri/renderD128`, using
8-bit NV12, constant QP 24, I and P pictures, and one reference picture. The driver
must support YUV420, constant-QP encoding and packed SPS/PPS/slice headers. The encoder
uploads the captured RGB frame and uses VA-API's video processor to convert it. If initialization fails,
OpenH264 provides a software Baseline encoder. If neither encoder works, the session
uses RemoteFX. These encoders are separate from Apple high-performance mode's HEVC
encoder.

Coded dimensions round up to multiples of 16. One region mask covers the visible
surface and crops the padding. The H.264 level covers both the coded area and each
dimension. SPS, PPS and an IDR start each stream. A size change
reactivates RDP, resets EGFX, creates and maps a new surface, and starts a new IDR.
Display Control and pointer scaling use the same visible dimensions as RemoteFX.

The server allows at most two unacknowledged frames. Cumulative acknowledgements
release earlier frames. If the client suspends acknowledgements, the bounded capture
queue and ten-frame-per-second limit pace delivery without retaining frame records.
A two-second ACK or decode-progress stall, or an encoder error, resets the surface
and encoder for a full refresh. A second failure uses the legacy codec for that connection.
The connection and input remain active. A closed EGFX channel also uses the legacy
path. Recovery cannot fix a client that closes the entire RDP connection.

Startup logs `RDP H.264 available` with the encoder name. Each connection logs
`RDP codec negotiated`, including the codec and reason. Legacy fallback logs use
`RemoteFX/bitmap`; `RDP legacy codec available` gives the actual legacy codec.
Connection logs include the peer address and session ID. Enable
`RUST_LOG=info,wayshare::rdp=debug` for `CapsAdvertise`, `CapsConfirm`, EGFX frame IDs,
ACKs and the client's decoded-frame count. FreeRDP can request this path with
`/gfx:avc420`; use `/gfx:RFX` for AVC-disabled EGFX and `-gfx /rfx` for legacy RemoteFX.
`RDP session summary` records the client and announced sizes, applied output mode,
codec and reason. Display Control caps and every received PDU appear at debug.

For an isolated loopback test, `WAYSHARE_TEST_RDP_FAIL_AFTER=20` injects one encoder
failure after 20 AVC420 frames in each connection. Network listeners ignore this
variable. It is unset in normal use.

## Shared login

Use the exact, case-sensitive username set by `wayshare setup` or `wayshare set-password`,
and leave the RDP domain empty. The username need not be a Linux account. Both protocols
load `srp_file` from the same config, defaulting to
`$XDG_CONFIG_HOME/wayshare/srp-$USER` or `~/.config/wayshare/srp-$USER`.
The file contains the username, salt, iteration count and SRP verifier. It contains no
password. RDP derives a verifier from the submitted password and compares it with that
same record. Changing the wayshare password changes both logins after restart.

The development ARD path, configured by `dev.dh_username` and `dev.dh_password_file`,
also shares its exact username and password checks with RDP. The password-file loader
trims trailing CR and LF bytes. When both Apple methods are configured, either credential
works. There is no PAM or keyring lookup. Linux account expiry and lockout policies do
not apply. The former `rdp.allowed_user` key has been removed; remove it from old configs
and use the Apple username in the client.

Credentials must pass before capture updates or input start. Failed attempts wait at
least two seconds. Password verification runs on blocking workers, and the delay affects
only the failed connection. At most eight connections are admitted, with a 30-second
login deadline. Submitted passwords are not saved or logged. Owned validation copies and
decoded credentials are cleared after use, including cancellation. This does not
guarantee that every temporary copy in process memory is erased.
TLS remains required. There is no NLA, CredSSP or smart-card login. A client that requires
NLA cannot connect.

Connect with FreeRDP 3, using the configured Apple username and the password prompt:

```sh
xfreerdp3 /v:127.0.0.1:3389 /sec:tls /u:alice /from-stdin:force
```

Verify the server certificate before supplying a real password. Do not use `/cert:ignore`
with a real password. A first connection over a trusted local path can establish trust;
remote access needs a trusted certificate or a fingerprint checked through another path.

## TLS identity

On first use, wayshare creates a self-signed localhost ECDSA certificate and key in one
PEM file, mode 0600. It reuses the identity on restart. The default path is
`$XDG_CONFIG_HOME/wayshare/rdp-identity.pem`, or `~/.config/wayshare/rdp-identity.pem`.
`identity_file` must be absolute. Invalid existing files cause an error and are not
replaced. For remote access, supply a certificate and PKCS#8 key with the right hostnames
or arrange explicit fingerprint verification.

At startup, wayshare logs the actual bound socket address and the TLS certificate's
SHA-256 fingerprint. A wildcard address such as `0.0.0.0` means all IPv4 interfaces;
enter the server's LAN or Tailscale IP in the client. The fingerprint hashes the full
DER certificate and uses 32 uppercase hex bytes separated by colons, for example
`BA:78:16:BF:...`. Compare it with the SHA-256 fingerprint in FreeRDP's certificate
prompt or the Microsoft client's certificate details. Letter case and separators can
differ; the bytes must match. A SHA-1 thumbprint is a different hash and cannot be
compared with this value. Do not accept a certificate whose SHA-256 fingerprint differs.

You can check the same value locally without exposing the private key:

```sh
openssl x509 -in /absolute/path/rdp-identity.pem -noout -fingerprint -sha256
```

## Input and capture

Keyboard scancodes map to Linux evdev codes through the shared input sink and keyboard
engine. PC modifiers bypass the Mac Command/Option remapping. Unicode input includes
UTF-16 surrogate pairs. With Super, Ctrl or Alt held, Unicode Space uses the physical
Space key on every layout. Printable US shortcut keys keep all held modifiers, including
Shift. Plain Unicode typing keeps its text-entry behavior. Other printable shortcuts
on non-US layouts still use the keysym path.

Absolute and relative pointer movement, five mouse buttons, and vertical and horizontal
wheel deltas use the shared pointer engine and Wayland virtual devices. `[input] enabled = false` disables input for both protocols. Disconnect
releases held keys and buttons. RFB and RDP share desktop focus, so concurrent input
from viewers can interfere.

Each authenticated connection owns a capture thread and a bounded frame queue. Capture
submits changed full frames at up to ten frames per second. Frames and pointer coordinates
use the negotiated client desktop size, including portrait sizes. The Wayland pointer
maps that extent onto the output's logical size, so output scale needs no extra multiplier.

Enable `resize = true` to fit the shared output to the client's requested pixel size.
RDP uses the same display owner as RFB and restores the original mode on disconnect.
Only one session can own output mode changes. The output keeps its current scale; mode
sizes may round down to whole logical pixels. Frames scale from the applied mode to
the requested desktop size. The same fitting also applies when resizing is disabled,
fails, or another session owns the output.

Display Control supports one primary monitor at the origin. Rotation requests can use
portrait or landscape dimensions. Width must be even, and both dimensions must be
between 200 and 8192 pixels, per
[MS-RDPEDISP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedisp/ea2de591-9203-42cd-9908-be7a55237d1c).
Dynamic layouts must also fit the advertised area of 8192 × 8192 pixels.
The RDP desktop preserves the client's exact initial dimensions, including odd widths
and heights. When the compositor rounds its mode, captured frames scale to the RDP
extent. Pointer coordinates use that same extent and clamp to its edges. A Display
Control resize announces the requested extent and rebuilds capture and the encoder.
A client that repeats its original size in Confirm Active does not undo that resize.
After 32 server reactivations, the session keeps its current extent and logs a warning.

Start with 1920x1080 or lower to limit CPU RemoteFX encoding work. The client must send
a Display Control layout or reconnect with a new desktop size to change the resolution.
Rotating a device can instead scale or crop the existing framebuffer locally.
With `RUST_LOG=info,wayshare::rdp=debug`, the server logs client capabilities, static and
dynamic channel states, completed Display Control caps writes, layout requests and
pointer extents. The caps allow one monitor with a maximum area of 8192 times 8192
pixels. Each dimension also has an 8192-pixel limit.

Microsoft's current [iOS display settings guide](https://learn.microsoft.com/en-us/windows-app/display-settings#display-settings)
places resolution under the app-wide Settings > Display, shared across connections.
On iPhone, enable Use Home Indicator Area to use the area reserved for the home indicator.
On iPad, the corresponding setting is Use Full Display. The guide describes default
letterboxing to protect rounded corners and system gestures. Per-PC display overrides,
Match device and a dynamic-resolution toggle are unconfirmed on iOS. The guide documents
similar controls on other platforms; their names and availability must not be assumed
on iPhone. Try reconnecting after selecting a resolution, then test rotation both ways.

Clipboard, sound and AVC444 remain unsupported.
Microsoft clients on Windows, iOS, Android and macOS need separate interoperability tests.

## Dependencies

The server enables its optional EGFX support. Other optional codecs and helpers remain disabled.
OpenH264 is built from source under its BSD license. Connector 0.10 unconditionally
links `picky` and `sspi` with smart-card support. Cargo's additive features cannot remove
that path. `deny.toml` therefore ignores only RUSTSEC-2023-0071 and RUSTSEC-2023-0089,
with reasons for each. The RSA and smart-card paths are unused by this TLS listener.
These exceptions do not establish that those dependencies are safe for other uses.

The local server dependency patch clears credentials promptly, rejects missing initial
credentials, and corrects pointer and Pause decoding. See its
[patch notes](../vendor/ironrdp-server/README.md). `mise run check` covers the default
build, `mise run check:no-rdp` covers the smaller build, and `mise run audit` checks both
default and all features.
