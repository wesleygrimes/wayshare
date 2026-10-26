# RDP

RDP streams the shared monitor over TLS with RemoteFX or compressed bitmap updates.
It uses the local Linux password through PAM. RDP is included in default builds but
its listener is disabled in config. The default address is `127.0.0.1:3389`.
Build with `--no-default-features` to omit RDP. PAM login requires Linux.

## Enable the listener

```toml
[rdp]
enabled = true
listen = "127.0.0.1:3389"
# identity_file = "/home/alice/.config/wayshare/rdp-identity.pem"
# allowed_user = "alice"
```

`--rdp`, `--no-rdp`, `--rdp-listen` and `--rdp-identity-file` override the corresponding
settings. A build without RDP rejects an enabled listener. RFB still needs its own
SRP verifier. Linux passwords do not replace RFB credentials.

Network addresses are allowed when explicitly configured. Keep the loopback default
and use an SSH tunnel until you have arranged server identity verification and network
access rules. RDP has no discovery advertisement. Port 3389 is the standard RDP port.

## Login and PAM packaging

The client must send the exact local username and an empty domain. Without
`allowed_user`, wayshare resolves the account for its real process UID, ignoring `$USER`.
`allowed_user` restricts login to that one account. It does not change the server's UID
or the desktop being shared. On Arch, non-root `pam_unix` can verify only the calling
user through `unix_chkpwd`. Run wayshare as the desktop owner and leave `allowed_user`
unset, or set it to that same account. Do not run the desktop server as root to allow
other users to log in.

Both PAM authentication and account checks must pass before capture updates or input
start. Empty passwords, nonempty domains, expired accounts and passwords that need
changing are rejected. Failed attempts wait at least two seconds. Each connection runs
separately, so one login's PAM work and delay do not block other connections. At most
eight connections are admitted, with a 30-second login deadline.

Packagers should install `contrib/pam.d/wayshare` as `/etc/pam.d/wayshare`, owned by root
and mode 0644. The supplied service includes Arch's `system-auth` for authentication
and account policy. Adapt the include names to the distribution's PAM stack. It opens
no login session and does not change process credentials. If the service file is absent,
wayshare uses `system-auth` if present, otherwise `login`. It never tries a second service
after a failed authentication. Distribution lockout and account policy still apply.

Passwords are used only for the PAM conversation. Wayshare does not save them to config,
files or logs. Owned validation copies and decoded credentials are cleared after use,
including cancellation. Transport and PAM may make their own temporary copies; this is
not a guarantee that every copy in process memory is erased. There is no NLA, CredSSP,
password database or smart-card login. A client or policy that requires NLA cannot connect.

Connect with FreeRDP 3, using its password prompt rather than a password argument:

```sh
xfreerdp3 /v:127.0.0.1:3389 /sec:tls /u:"$(id -un)" /from-stdin:force
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

## Input and capture

Keyboard scancodes map to Linux evdev codes through the shared input sink and keyboard
engine. PC modifiers bypass the Mac Command/Option remapping. Unicode input includes
UTF-16 surrogate pairs. Absolute and relative pointer movement, five mouse buttons,
and vertical and horizontal wheel deltas use the shared pointer engine and Wayland
virtual devices. `[input] enabled = false` disables input for both protocols. Disconnect
releases held keys and buttons. RFB and RDP share desktop focus, so concurrent input
from viewers can interfere.

Each authenticated connection owns a capture thread and a bounded frame queue. Capture
submits changed full frames at up to ten frames per second. A monitor size change ends
the session. Restart wayshare after changing the monitor mode. Large desktops can be
slow with CPU encoding.

Clipboard, sound, monitor resizing and hardware RDP video encoding remain unsupported.
Microsoft clients on Windows, iOS, Android and macOS need separate interoperability tests.

## Dependencies

The server's optional codecs and helpers remain disabled. Connector 0.10 unconditionally
links `picky` and `sspi` with smart-card support. Cargo's additive features cannot remove
that path. `deny.toml` therefore ignores only RUSTSEC-2023-0071 and RUSTSEC-2023-0089,
with reasons for each. The RSA and smart-card paths are unused by this TLS/PAM listener.
These exceptions do not establish that those dependencies are safe for other uses.

The local server dependency patch clears credentials promptly, rejects missing initial
credentials, and corrects pointer and Pause decoding. See its
[patch notes](../vendor/ironrdp-server/README.md). `mise run check` covers the default
build, `mise run check:no-rdp` covers the smaller build, and `mise run audit` checks both
default and all features.
