# Troubleshooting

Start with `wayshare doctor`. It checks Hyprland, the monitor, the password, the service,
the port, ufw and the High Performance encoder, and says how to fix each failure.

For anything it doesn't catch, read the log:

```sh
journalctl --user -u wayshare -f
```

## The service doesn't start at login

The service starts with `graphical-session.target`. Omarchy and other setups that launch
Hyprland through UWSM reach that target; a plain `Hyprland` launch doesn't. Start it from
`hyprland.conf` instead:

```
exec-once = systemctl --user start wayshare
```

## "no output set and there are 2 monitors"

wayshare only picks a monitor for you when there's one. Run `wayshare setup` to choose, or
set `output` in the config.

## "wayshare needs Hyprland 0.56 or newer"

Upgrade Hyprland to 0.56 or newer.

## The Mac can't connect

- Check the firewall: TCP 5900 has to be open. See [Firewall](firewall.md).
- Screen Sharing allows one window per address. Close any open window for this machine,
  including ones behind other windows, and dismiss any leftover alerts.

## The password is rejected

Set it again and restart:

```sh
wayshare set-password
wayshare restart
```

## No High Performance

Look for `High Performance mode disabled` or `HP encoder` near the top of the log, then see
[High Performance mode](high-performance.md).

## The machine doesn't show up in Finder's sidebar

mDNS needs UDP 5353 open, and `[mdns] enabled = true` (the default). You can always connect
by address instead.
