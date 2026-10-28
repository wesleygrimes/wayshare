# Local server patch

This directory contains the published `ironrdp-server` 0.13.0 source, licensed under
MIT OR Apache-2.0. The upstream source is available at
https://docs.rs/crate/ironrdp-server/0.13.0/source/.

Local changes:

- Clear decoded passwords immediately after credential validation, including cancellation.
- Wrap access-denied errors in the Share Control and Share Data headers clients expect.
- Reject missing credentials on the initial connection when a validator is configured.
- Preserve pointer coordinates and simultaneous button changes. Decode middle and side
  buttons, horizontal wheel deltas and the Pause scancode prefix.
- Notify the display handler of the negotiated size even without client resize support,
  and process queued input after the display extent is set.
- Log negotiated static channels, client capabilities and channel responses under the
  application RDP debug target. Log when Display Control caps enter the output queue.
- Carry EGFX DVC output through the bounded display queue so frames, resets and legacy
  fallback remain ordered across reactivation. Log the available legacy codec at info.
- Advertise 16 Display Control monitors by default with area factors of 8192 by 8192,
  and log every received Display Control PDU before decoding it.
- Keep negotiation flags 0x0b with EGFX and create Graphics first with ids from 1.
- Send Logon Info Version 2 once after activation and successful credential validation,
  with the accepted username, empty domain and a process-local logon ID. Windows App
  needs this notification before it sends live rotation layouts.
- Queue bounded control replies separately from graphics completion so reads continue
  during a pending write. Close stalled writes after five seconds.
- Honor Suppress Output and keep control traffic active. Resume requests a full frame.
  Display Control layouts apply while suppressed. Suppression never starts a probe or
  close timer. Each activation has a five-second deadline.
- Carry surface lifecycle commands in the graphics queue, bypassing suppression.
- Dispatch control PDUs during activation and retain virtual channels on the server.
  Use the connector sequence API to keep activation and control processing separate.
- Report the selected legacy codec to the display handler for session summaries.
- Pad compressed bitmap scanlines to four pixels without enlarging their destination.
  Count cropped rows by their declared height, including the final partial stride.
- Expose the existing RemoteFX encoder for EGFX CAVIDEO, with complete stream headers.
- Set RemoteFX CONTEXT and TILESET to video mode, as required by MS-RDPRFX 2.2.2.3.4.
- Use project punctuation in imported comments.
- Add `zeroize` and use a local package manifest without the upstream lint configuration.

The application tests exercise the modified input conversions through the public API.
The live authentication proof checks rejection before streaming. Recheck these changes
when updating the dependency. The rest of the upstream implementation is unchanged.

CredSSP dependencies remain unconditional in connector 0.10. This patch does not remove
them or enable them in the application. The application enables EGFX. Other optional server features remain off.
