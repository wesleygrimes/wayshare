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
- Use project punctuation in imported comments.
- Add `zeroize` and use a local package manifest without the upstream lint configuration.

The application tests exercise the modified input conversions through the public API.
The live authentication proof checks rejection before streaming. Recheck these changes
when updating the dependency. The rest of the upstream implementation is unchanged.

CredSSP dependencies remain unconditional in connector 0.10. This patch does not remove
them or enable them in the application. Optional server features remain off.
