# High Performance mode

High Performance is Screen Sharing's low-latency mode. It streams HEVC video and audio over
UDP and adjusts the bitrate to your network. Connections that don't ask for it get the
regular Standard mode.

## What it needs

wayshare picks an encoder when it starts:

1. VA-API, when the driver for the first GPU (`/dev/dri/renderD128`) can encode HEVC. Needs
   `libva` and that driver.
2. x265 otherwise. Needs `libx265` with API build 217.

`wayshare doctor` shows which one it chose, or why High Performance is unavailable. Without
an encoder, wayshare still runs in Standard mode.

NVIDIA's NVENC isn't supported yet, so NVIDIA machines use x265.

Audio needs `libfdk-aac`, the `libpulse` client library, and a PulseAudio-compatible server
such as PipeWire's `pipewire-pulse`. Without them, video works but there is no sound.

## How it behaves

- One High Performance viewer at a time. Later viewers get Standard mode, except a reconnect
  from the same address, which takes over.
- If the viewer sends no feedback within 15 seconds of starting, or goes quiet for 10 seconds
  later, the session falls back to Standard mode.
- When a session ends, including a dropped network or a session that crashes, wayshare
  releases held keys and buttons, stops the stream and restores the monitor. If the whole
  process is killed, the monitor keeps the Mac's mode until wayshare runs again.
