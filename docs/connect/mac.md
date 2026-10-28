# Connect from your Mac

1. [Install wayshare](../../README.md#install) on your Hyprland machine.
2. In a terminal inside your Hyprland session, run:

   ```sh
   wayshare setup
   ```

   Choose your password and monitor. Setup starts the service and shows the
   address and username. Accept its ufw rule for TCP 5900 if needed.
3. On your Mac, open **Screen Sharing**.
4. Choose **Connection > New** and enter the address from setup.
5. Sign in with the username from setup and your wayshare password.
6. Select **High Performance** when prompted. If it is unavailable, use
   **Standard** and check the [High Performance requirements](../high-performance.md).

You see your shared Hyprland desktop. Command sends Super by default, so your
Hyprland shortcuts work. You can copy text, rich text, and images between machines.
High Performance also carries sound when the server has the required audio libraries.

Apple Screen Sharing does not use an RDP certificate or fingerprint check. Standard
screen updates and clipboard data are unencrypted. Use a trusted LAN or VPN. See
[Security](../security.md) for encryption and ports.

For connection problems, follow [Mac troubleshooting](../troubleshooting.md#mac).
To use RDP on the Mac instead, follow the [Windows App quickstart](windows-app.md).
RDP has no clipboard or sound.
