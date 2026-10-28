# Local acceptor patch

This directory contains the published `ironrdp-acceptor` 0.10.0 source, licensed under
MIT OR Apache-2.0. The upstream source is available at
https://docs.rs/crate/ironrdp-acceptor/0.10.0/source/.

`create_gcc_blocks` advertises server core version `0x0008000C`, RDP 10.7, instead
of `0x00080004`. Microsoft lists these values in
[Server Core Data](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/379a020e-9925-4b4f-98f3-7d634e10b411).
The acceptor has no version configuration hook.

Local changes also add per-acceptor negotiation flags. Monitor Layout uses zero
`pduSource`, from upstream commit `3ca12ee`, and inclusive right and bottom bounds,
from `063efcdc`. Unit tests check the encoded bytes. Handshake diagnostics use the application RDP info target.
Client Info diagnostics are omitted because that PDU contains credentials.
Finalization rejects early input above 256 packets or 1 MiB before copying it.
Credentials are checked after finalization, so this bounds unauthenticated retention.
Unit tests cover both limits and normal completion with queued input.
The local manifest omits the upstream lint configuration, as the server patch does.

Recheck this field when updating the dependency. The application selects codecs
from client capabilities, independently of the advertised server version.
