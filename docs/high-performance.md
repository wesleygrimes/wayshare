# High Performance mode

High Performance is Screen Sharing's low-latency mode. It streams HEVC video and audio over
UDP and adjusts the bitrate to your network. Connections that don't ask for it get the
regular Standard mode.

## Connecting

In the Screen Sharing app, connect to your machine's address and choose **High Performance**
when it asks for a connection type. You can also switch it for an open connection from the
toolbar.

To open it directly instead, enter this in Finder's **Go > Connect to Server**:

```
vnc://192.168.1.20?quality=high&numVirtualDisplays=1
```

## What it needs

wayshare picks an encoder when it starts:

1. VA-API, when the driver for the first GPU (`/dev/dri/renderD128`) can encode HEVC. Needs
   `libva` and that driver.
2. x265 otherwise. Needs `libx265` with API build 217.

The log says which one it chose:

```
HP encoder backend=Vaapi
```

If neither works, wayshare still runs in Standard mode and logs why:

```
High Performance mode disabled: no hardware HEVC encoder, and the libx265 fallback is unusable: ...
```

NVIDIA's NVENC isn't supported yet, so NVIDIA machines use x265.

Audio needs `libfdk-aac`, the `libpulse` client library, and a PulseAudio-compatible server
such as PipeWire's `pipewire-pulse`. Without them video still works, and the log says
`HP audio unavailable, continuing with video only`.

## How it behaves

- One High Performance viewer at a time. Later viewers get Standard mode, except a reconnect
  from the same address, which takes over.
- If the viewer sends no feedback within 15 seconds of starting, or goes quiet for 10 seconds
  later, the session falls back to Standard mode.
- When a session ends, including a dropped network or a session that crashes, wayshare
  releases held keys and buttons, stops the stream and restores the monitor. If the whole
  process is killed, the monitor keeps the Mac's mode until wayshare runs again.
