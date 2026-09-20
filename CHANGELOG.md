# Changelog

## 0.1.1

- Distribute source instead of a prebuilt executable. Build locally with
  `bin/build` before enabling the plugin and after every update.
- Package source archives with the locked Rust dependencies and build script.
- Show a build instruction when the backend has not been compiled.

## 0.1.0

First release.

- Share new text copies with one authorized Android phone over USB or Wi-Fi.
- Pair over Wi-Fi using a QR code, with a manual connection fallback.
- Compact Omarchy panel with keyboard navigation and visible focus.
- Preserve existing clipboards on connect/resume, remember the user's enabled or
  paused choice, and automatically resume enabled sync after temporary failures
  or service restarts.
- Use system-installed scrcpy 4.1, Android tools, Avahi, and qrencode.
- Ship the prebuilt x86-64 bridge in the plugin repository so installation and
  updates do not require a Rust toolchain.
- Include dependency checks and an architecture-specific release archive builder.
