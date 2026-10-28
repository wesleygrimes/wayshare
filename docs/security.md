# Security

wayshare accepts Apple Screen Sharing and RDP connections on your LAN or VPN.
Both listeners default to all IPv4 interfaces, and RDP follows the Apple address. Router port forwarding exposes
these services to the internet. A VPN such as [Tailscale](https://tailscale.com)
provides remote access without port forwarding.

## Login

Both protocols use the Apple-mode username and the password you choose in
`wayshare setup`. `wayshare set-password` stores a salt and verifier in `srp-NAME`,
not the password. The username need not match a Linux account. Someone who copies
that file can guess passwords against it offline. A separate wayshare password
limits the damage if that happens.

Apple Screen Sharing uses an SRP proof. The Mac proves it knows the password
without sending it. RDP sends the password inside TLS, and wayshare checks it
against the same verifier. RDP requires an exact, case-sensitive username and an
empty domain. It has no NLA, CredSSP, PAM, keyring, or smart-card login.

RDP accepts at most eight connections and gives each login 30 seconds. Failed
logins wait at least two seconds. Before login, wayshare limits queued input to
256 packets and 1 MiB. It sends no screen updates and accepts no desktop input
until credentials pass.

If the SRP record or TLS identity is unusable, RDP stays off and logs a repair
message. Apple Screen Sharing keeps listening and still requires credentials.

## Encryption

| Traffic | Apple Screen Sharing | RDP |
|---|---|---|
| Login | SRP proof. The password is never sent | Password inside TLS |
| Keyboard and mouse | Encrypted by the Mac | TLS |
| Standard screen updates | Unencrypted | TLS |
| Clipboard | Unencrypted | Unsupported |
| High Performance video and sound | SRTP | Unsupported. RDP graphics use TLS |

Anyone who can watch your network can see Apple Standard screen updates and
clipboard data. A VPN encrypts that traffic too.

## Certificate identity

RDP creates a self-signed certificate and private key on first use. The default
file is `~/.config/wayshare/rdp-identity.pem`, or
`$XDG_CONFIG_HOME/wayshare/rdp-identity.pem`. wayshare creates it with mode 0600
and reuses it across restarts. An invalid existing file causes an error. wayshare
does not replace it. The certificate names are `localhost`, `127.0.0.1`, and `::1`,
so clients warn when you connect by a LAN address.

The startup log contains the certificate's SHA-256 fingerprint as colon-separated
hex bytes. Comparing that value with the client certificate details through a
trusted path establishes trust on first use. A SHA-1 thumbprint uses a different
hash and cannot be compared with SHA-256. Accepting a warning without checking
identity can send your password to another server.

The certificate file provides the same SHA-256 fingerprint through a trusted
connection to the server:

```sh
openssl x509 -in ~/.config/wayshare/rdp-identity.pem -noout -fingerprint -sha256
```

That command reads the default file. A custom `rdp.identity_file` needs its own
path. For a client that shows only a SHA-1 thumbprint, the same certificate file
provides a matching SHA-1 value:

```sh
openssl x509 -in ~/.config/wayshare/rdp-identity.pem -noout -fingerprint -sha1
```

A certificate the client trusts, with the right hostnames and a PKCS#8 key in
`rdp.identity_file`, avoids reliance on the warning. See the
[RDP settings](rdp.md#settings).

## VPN access

The network choice in `wayshare setup` applies to both protocols. Selecting
`tailscale0` binds Apple Screen Sharing to your Tailscale address and RDP to the
same address on port 3389. An explicit `[rdp] listen` overrides this.
`[rdp] enabled = false` turns RDP off.

Tailscale must be up before wayshare binds its address. An Apple listener bind
failure stops the service. An RDP bind failure leaves Apple Screen Sharing
running. A restart after Tailscale comes up retries the listeners.

## Ports

| Default port | Use | Inbound firewall rule |
|---|---|---|
| TCP 5900 | Apple Screen Sharing | Required for Apple clients |
| TCP 3389 | RDP over TLS, enabled by default | Required for RDP clients |
| UDP 5353 | mDNS for Finder discovery | Optional. Not used for RDP discovery |
| UDP 5960 and 5962 | Apple High Performance video and sound | Not needed. wayshare sends first, so replies use connection tracking |

`listen`, `rdp.listen`, and `hp.port` can change these ports. High Performance
sound uses `hp.port + 2`.

Setup offers ufw rules for TCP 5900 and the enabled RDP port, normally 3389.
Existing installs need another setup run or a TCP 3389 rule for the LAN or VPN.
Other firewalls need their own rules. RDP has no mDNS advertisement.

`wayshare doctor` checks the Apple listener, its ufw rule, and the ufw rule for
the RDP port. It does not check RDP connectivity or the RDP certificate.
