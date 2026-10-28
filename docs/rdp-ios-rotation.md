# iOS Windows App rotation

The iOS Windows App needs Save Session Info after logon before it sends Display
Control Monitor Layout PDUs when the phone rotates. Wes verified this on an iPhone
on 2026-10-28. Planar EGFX frames already worked; the missing logon notification
kept rotation from producing layouts.

The successful test used plain TLS and the normal shared login with only the
Save Session Info probe enabled. It did not need NLA or auto-detect. Auto-detect
caused phone error `0x1304`. The early-flags and General/Surface capability probes
did not enable rotation.

wayshare now sends the notification once after activation and credential
validation, before starting the virtual channels. It uses
[Logon Info Version 2](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/a2e946b4-820c-4d11-b4c9-8029a7df558f),
`INFOTYPE_LOGON_LONG = 0x00000001`, with the accepted username, an empty domain and
a session ID assigned within the server process. Windows sends a logon notification
at this stage; see [MS-RDPBCGR 3.3.5.10.1](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/3061e2df-ae7c-41fe-8743-650a9cb70365).
No compatibility environment switches are needed.

The phone sent `orientation = 0` with its 836x1580 portrait layout. IronRDP reads
the field correctly and logs `Landscape`, which the protocol defines as no
rotation. It describes a rotation angle, not whether height exceeds width.
wayshare uses the supplied dimensions without swapping them.

To verify a connection, look for `RDP Save Session Info sent` with the login's
username, `RDP Display Control caps advertised`, and `RDP EGFX frame ACK`.
Rotate both ways and check for `RDP Monitor Layout received`, the new dimensions,
and `RDP EGFX ResetGraphics`. Suppress Output still pauses frames while the app is
hidden. A layout can resize the surface during suppression; resume repaints it.
