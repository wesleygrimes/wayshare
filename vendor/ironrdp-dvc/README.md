# Local DVC patch

This directory contains the published `ironrdp-dvc` 0.8.0 source, licensed under
MIT OR Apache-2.0. The upstream source is available at
https://docs.rs/crate/ironrdp-dvc/0.8.0/source/.

`DrdynvcServer::with_first_channel_id` lets the application choose the first channel
id before registering channels. The existing constructor still starts at 0.
Creation logs list channel names and ids under the application RDP info target.
Tests cover wire ids, typed lookup, channel close callbacks, and allocation after close.
The local manifest enables unit tests and omits the upstream lint configuration.
Recheck these changes when updating the dependency.
