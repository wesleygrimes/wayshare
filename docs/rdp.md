# RDP

RDP streams the shared monitor over TLS with RemoteFX or compressed bitmap updates.
It uses the same username and password as Apple Screen Sharing, including high
performance mode. RDP is included in the default Cargo features. Its runtime listener
is disabled by default, with `127.0.0.1:3389` as the default address.
Build with `--no-default-features` to omit RDP.

## Enable the listener

```toml
[rdp]
enabled = true
listen = "127.0.0.1:3389"
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
sizes may round down to whole logical pixels. After a successful resize, the session
uses the applied mode's dimensions for frames and pointer coordinates. If resizing is
disabled, fails, or another session owns the output, frames scale to the client size.

Display Control supports one primary monitor at the origin. Rotation requests can use
portrait or landscape dimensions. Width must be even, and both dimensions must be
between 200 and 8192 pixels, per
[MS-RDPEDISP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedisp/ea2de591-9203-42cd-9908-be7a55237d1c).
Dynamic layouts must also fit the advertised area of 3840 × 2400 pixels.
An odd initial desktop width rounds down. A size change reactivates the RDP session
and rebuilds capture and the encoder before sending frames at the new size.

Start with 1920x1080 or lower to limit CPU RemoteFX encoding work. The client must send
a Display Control layout or reconnect with a new desktop size to change the resolution.
Rotating a device can instead scale or crop the existing framebuffer locally.
With `RUST_LOG=info,wayshare::rdp=debug`, the server logs client capabilities, static and
dynamic channel states, completed Display Control caps writes, layout requests and
pointer extents. The caps allow one monitor with a maximum area of 3840 times 2400
pixels, not a separate width limit of 3840. Both phone orientations fit that area.

Microsoft's current [iOS display settings guide](https://learn.microsoft.com/en-us/windows-app/display-settings#display-settings)
places resolution under the app-wide Settings > Display, shared across connections.
On iPhone, enable Use Home Indicator Area to use the area reserved for the home indicator.
On iPad, the corresponding setting is Use Full Display. The guide describes default
letterboxing to protect rounded corners and system gestures. Per-PC display overrides,
Match device and a dynamic-resolution toggle are unconfirmed on iOS. The guide documents
similar controls on other platforms; their names and availability must not be assumed
on iPhone. Try reconnecting after selecting a resolution, then test rotation both ways.

Clipboard, sound and hardware RDP video encoding remain unsupported.
Microsoft clients on Windows, iOS, Android and macOS need separate interoperability tests.

## Dependencies

The server's optional codecs and helpers remain disabled. Connector 0.10 unconditionally
links `picky` and `sspi` with smart-card support. Cargo's additive features cannot remove
that path. `deny.toml` therefore ignores only RUSTSEC-2023-0071 and RUSTSEC-2023-0089,
with reasons for each. The RSA and smart-card paths are unused by this TLS listener.
These exceptions do not establish that those dependencies are safe for other uses.

The local server dependency patch clears credentials promptly, rejects missing initial
credentials, and corrects pointer and Pause decoding. See its
[patch notes](../vendor/ironrdp-server/README.md). `mise run check` covers the default
build, `mise run check:no-rdp` covers the smaller build, and `mise run audit` checks both
default and all features.
