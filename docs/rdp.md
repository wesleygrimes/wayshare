# RDP prototype

The optional RDP adapter streams the shared monitor over TLS using compressed bitmap
updates. It negotiates RemoteFX when the client supports it and otherwise uses the
server's bitmap fallback. It is view-only, has no authentication and requires a loopback listen address.
Any local process can view the screen. Remote viewers need an SSH tunnel. RFB and High
Performance mode keep their existing settings and behavior.

Build it with:

```sh
cargo build --locked --features rdp
```

Enable it in a separate test config:

```toml
listen = "127.0.0.1:15900"
srp_file = "/absolute/path/to/test-srp"
output = "DP-1"
resize = false

[rdp]
enabled = true
listen = "127.0.0.1:13389"
identity_file = "/absolute/path/to/test/rdp-identity.pem"

[hp]
enabled = false

[mdns]
enabled = false
```

Create a separate SRP verifier with `wayshare set-password --srp-file PATH` for the RFB
listener. Then run `target/debug/wayshare --config /absolute/path/to/test/config.toml`.
Choose unused ports and the output name shown by `hyprctl monitors`.

`--rdp` and `--no-rdp` override `rdp.enabled`. `--rdp-listen` and
`--rdp-identity-file` override the other RDP settings. Enabling RDP without the Cargo
feature returns an error.

On first use, wayshare generates a self-signed localhost certificate and key in one PEM
file. It creates the file with mode 0600 on Unix and reuses it on later starts. Without
`identity_file`, the path is `$XDG_CONFIG_HOME/wayshare/rdp-identity.pem`, or
`~/.config/wayshare/rdp-identity.pem`. Invalid existing files cause an error. They are
never replaced automatically. The path must be absolute.

Connect with FreeRDP 3 for a local test:

```sh
xfreerdp3 /v:127.0.0.1:13389 /sec:tls /cert:ignore /u:viewer /p:unused
```

`/cert:ignore` skips server identity verification and is for local testing. The username
and password are placeholders and are not checked. NLA is not implemented in this adapter.
Do not reuse a real password.

The prototype serves one RDP client at a time. It captures at up to ten frames per second
and submits full frames when pixels change. Capture runs in a separate thread with a
bounded queue. Disconnect and process shutdown release that thread. Monitor size changes
end the session; restart the prototype after changing the monitor mode.

Input, clipboard, sound, monitor resizing and hardware RDP video encoding remain future
work. Windows, iOS, Android and macOS clients need separate interoperability tests before
support can be claimed. Run `mise run check:rdp` to check the optional code.

The additional `cargo deny --locked --all-features check` audit reports
[an unmaintained atomic-polyfill dependency](https://rustsec.org/advisories/RUSTSEC-2023-0089.html)
and [an RSA timing vulnerability](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
in the authentication dependency graph. No advisory exceptions are configured. This
adapter uses rustls with AWS-LC and an ECDSA identity for TLS, and does not enable the
RSA-based authentication paths. Review these findings before adding NLA or network access.
