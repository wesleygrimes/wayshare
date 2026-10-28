# Troubleshooting

## Start here

Run these commands inside your Hyprland session:

```sh
wayshare doctor
journalctl --user -u wayshare -b
```

Doctor checks the shared setup, the Apple listener and its firewall rule, and the
Apple High Performance encoder. It does not check RDP. This is a known gap.
For RDP, check the startup log for `RDP listener ready` or `RDP disabled`.

To follow the log during a connection attempt, run:

```sh
journalctl --user -u wayshare -f
```

## Shared problems

### The service does not start at login

The service starts with `graphical-session.target`. Omarchy and other setups that
launch Hyprland through UWSM reach that target. A plain `Hyprland` launch does not.
For a plain launch, add this to `hyprland.conf`:

```text
exec-once = systemctl --user start wayshare
```

If the service still fails, read the log for a config error or missing monitor.
Run `wayshare setup` inside your Hyprland session to pick an enabled monitor.

### The password is rejected

Use the username from setup and your wayshare password. To reset the password, run:

```sh
wayshare set-password
wayshare restart
```

If you set `srp_file` or a custom username, use the matching `--srp-file` and
`--user` options. See [Login configuration](configuration.md#login).
For RDP, also check the [username and domain](#rdp-login-is-rejected).

## Mac

### Screen Sharing cannot connect

Close any open Screen Sharing window for this machine, including windows behind
other windows. Dismiss leftover alerts. Screen Sharing allows one window per
address. Try the [Mac quickstart](connect/mac.md) again.

If the connection still fails, check the Apple port and firewall with
`wayshare doctor`.

### The machine does not appear in Finder

Allow UDP 5353 on your LAN and check that `[mdns] enabled = true`, the default.
You can also connect by address through **Connection > New** in Screen Sharing.
RDP clients use an address. wayshare does not advertise RDP through mDNS.

### High Performance is unavailable or falls back

Run `wayshare doctor` to check the encoder. Install the missing libraries listed
in [High Performance mode](high-performance.md). Only one High Performance viewer
can connect at a time. Later viewers get Standard mode, except a reconnect from
the same address, which takes over.

If video starts and then falls back, check the network path for UDP feedback.
wayshare falls back after 15 seconds without initial feedback or 10 seconds of
silence during a session. For missing sound, check the audio requirements on the
High Performance page. RDP has no sound support.

## RDP

### The connection is refused or times out

Check the startup log for `RDP listener ready`. Default builds enable RDP at
`0.0.0.0:3389`. If you turned it off, remove `--no-rdp` or set `[rdp] enabled = true`.
If `rdp.listen` uses loopback, set it to your LAN or VPN address. The top-level
`listen` setting controls Apple Screen Sharing only.

For `RDP disabled`, follow the logged repair message. A missing SRP record needs
`wayshare set-password`. A TLS identity error needs a valid `rdp.identity_file`
and directory permissions that let wayshare read or create it. Then run
`wayshare restart`.

If the listener is ready, allow TCP 3389 from your LAN or VPN in your firewall.
Setup offers the ufw rule. On an existing install, rerun `wayshare setup` or add
that rule yourself. Doctor does not check this port or rule. Builds with
`--no-default-features` omit RDP.

### The client shows a certificate warning

Check the certificate's SHA-256 fingerprint against the startup log before you
send your password. Stop if it differs. A SHA-1 thumbprint cannot match SHA-256.
Follow [Certificate identity](security.md#certificate-identity) if the client
shows only SHA-1 or if you need a trusted certificate.

### RDP login is rejected

Enter the exact, case-sensitive Apple-mode username from setup and leave the
domain empty. Remove any saved domain, computer-name prefix, or Microsoft account.
Use your wayshare password. Login uses TLS without NLA or CredSSP.

If that fails, follow [The password is rejected](#the-password-is-rejected).

### The screen stays black or graphics fail

Check the log for the negotiated codec and any fallback. To try the legacy
RemoteFX or bitmap path, set this in `~/.config/wayshare/config.toml`:

```toml
[rdp]
codec = "remotefx"
```

If `[rdp]` already exists, change its `codec` key instead of adding another table.
Run `wayshare restart` and reconnect. See [RDP graphics](rdp.md#graphics) for the
other codec values. Avoid `egfx-remotefx` with the iOS Windows App. It causes
protocol errors.

For more logs, set `RUST_LOG=info,wayshare::rdp=debug` in the service environment.
One way to do that is:

```sh
systemctl --user edit wayshare
```

Add this override, then restart wayshare:

```ini
[Service]
Environment=RUST_LOG=info,wayshare::rdp=debug
```

```sh
wayshare restart
journalctl --user -u wayshare -f
```

### Rotation or resize does not work

Check that top-level `resize = true`, the default, and reconnect. Disconnect other
viewers if one owns the monitor's size changes. In the log, look for
`RDP Monitor Layout received` with the requested dimensions. If there is no layout,
the client has not requested a resize.

Live resize and rotation are verified on Windows App on iPhone only. Other tested
RDP clients have verified connections. See [Client compatibility](clients.md) and
the [resize limits](rdp.md#resizing-and-input).
