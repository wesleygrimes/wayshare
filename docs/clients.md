# Client compatibility

The maintainer has tested the clients marked as tested below. A connection test
does not verify every client option.

| Client | Protocol | Status | Verified behavior |
|---|---|---|---|
| Mac Screen Sharing, Standard | Apple Screen Sharing | Tested | Connects with text, rich text, and image clipboard |
| Mac Screen Sharing, High Performance | Apple Screen Sharing | Tested | Connects with HEVC video and sound. One High Performance viewer at a time |
| Windows App on iPhone | RDP | Tested | Connects with live resize and rotation |
| Windows App on iPad | RDP | Tested | Connects |
| Windows App on Mac | RDP | Tested | Connects |
| Windows Remote Desktop Connection (mstsc) | RDP | Tested | Connects |
| FreeRDP (xfreerdp3) | RDP | Tested | Connects |
| Generic VNC clients | VNC | Rejected | The Apple listener accepts only Apple RFB 3.888 and 3.889 |

The [iPhone rotation note](development/rdp-ios-rotation.md) records the live test.
Live resize and rotation remain unverified on the other RDP clients in the table.
Windows App on Windows is untested.

## Protocol limits

| Protocol | Limits |
|---|---|
| Apple Screen Sharing | One High Performance viewer at a time. Later viewers use Standard mode, except a reconnect from the same address, which takes over |
| RDP | No clipboard, sound, or AVC444. TLS login only, without NLA or CredSSP |
| RDP display layout | One monitor from each client layout. Dimensions from 200 to 8192 pixels, with even layout widths |
| Both | Viewers share desktop focus. One session owns monitor size changes at a time |

Connection steps are in the [Mac](connect/mac.md),
[Windows App and mstsc](connect/windows-app.md), and [FreeRDP](connect/freerdp.md)
quickstarts. [RDP](rdp.md) and [High Performance mode](high-performance.md) describe
the protocol limits in more detail.
