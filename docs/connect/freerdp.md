# Connect with FreeRDP

1. [Install wayshare](../../README.md#install) on your Hyprland machine.
2. In a terminal inside your Hyprland session, run:

   ```sh
   wayshare setup
   ```

   Choose your password and monitor. Setup starts the service and shows your
   Apple-mode username. Accept the ufw rule for TCP 3389 if needed. On an existing
   install, rerun setup or allow TCP 3389 from your LAN or VPN.
3. On the Hyprland machine, read the server's SHA-256 certificate fingerprint:

   ```sh
   journalctl --user -u wayshare -b
   ```

   Find the latest `RDP TLS certificate fingerprint` line. Copy the colon-separated
   hex value through a trusted path to your client machine.
4. On your client machine, install FreeRDP 3 with `xfreerdp3` through your package
   manager. Replace `ADDRESS`, `USERNAME`, and `FINGERPRINT` below, then run:

   ```sh
   xfreerdp3 /v:ADDRESS:3389 /sec:tls /u:USERNAME \
     /cert:fingerprint:sha256:FINGERPRINT /from-stdin:force
   ```

   Use your Hyprland machine's address and the exact Apple-mode username from
   setup. Leave the domain empty and omit `/d:` and any domain prefix. Enter your
   wayshare password at the prompt. The pinned SHA-256 fingerprint checks the
   certificate before FreeRDP sends credentials. Do not use `/cert:ignore`.

You see your shared Hyprland desktop and can control it. FreeRDP connections are
tested. Live resize and rotation remain unverified with this client.

RDP is on by default at `0.0.0.0:3389`. It has no clipboard or sound. For codec
options, see [RDP graphics](../rdp.md#graphics). For connection problems, follow
[RDP troubleshooting](../troubleshooting.md#rdp).
