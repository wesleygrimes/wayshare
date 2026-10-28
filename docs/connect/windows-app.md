# Connect with Windows App or mstsc

Use these steps for Windows App on iPhone, iPad, or Mac, or Windows Remote Desktop
Connection (`mstsc`) on Windows. See [Client compatibility](../clients.md) for
verified behavior.

1. [Install wayshare](../../README.md#install) on your Hyprland machine.
2. In a terminal inside your Hyprland session, run:

   ```sh
   wayshare setup
   ```

   Choose your password and monitor. Setup starts the service and shows your
   Apple-mode username. RDP uses that same username and password. Accept the ufw
   rule for TCP 3389 if needed. On an existing install, rerun setup or allow TCP
   3389 from your LAN or VPN.
3. On the Hyprland machine, read the server's SHA-256 certificate fingerprint:

   ```sh
   journalctl --user -u wayshare -b
   ```

   Find the latest `RDP TLS certificate fingerprint` line. If you changed
   `rdp.identity_file`, the fingerprint belongs to that file.
4. Open your client. In Windows App, add a PC. In `mstsc`, enter a computer
   address. Use your Hyprland machine's IP address with port 3389, such as
   `192.168.1.50:3389`. RDP is on by default, so no config edit is needed.
5. Connect with the exact Apple-mode username from setup. Leave the domain empty.
   Do not add a computer name, domain prefix, or Microsoft account to the username.
6. Before you send your wayshare password, inspect the server certificate.
   Compare its SHA-256 fingerprint with the server log. A SHA-1 thumbprint does
   not match a SHA-256 fingerprint. If the client shows only SHA-1, use the
   server command in [Certificate identity](../security.md#certificate-identity)
   to get the matching thumbprint through a trusted connection. Accept the
   certificate only after you verify it, then sign in with your wayshare password.

You see your shared Hyprland desktop and can control it. On iPhone, live resize
and rotation work during the session. The other clients listed here have verified
connections. Resize and rotation remain unverified on them.

RDP has no clipboard or sound. For connection problems, follow
[RDP troubleshooting](../troubleshooting.md#rdp).
