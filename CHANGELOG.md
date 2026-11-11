# Changelog

## Unreleased

- Send a short text (a link, a password, a snippet) to a paired device: **Send text…** on a device card, with
  **Paste** from the clipboard, and **Copy** on the receiving side. `lanshare send-text` in the terminal, reading
  standard input when no text is given. Texts are never saved or logged.
- Wire protocol version 7 adds `TextOffer`. Version 6 devices (LanShare 1.0) still send and receive files with this
  version; only texts need both devices updated.
- Chunk size for outgoing transfers, 1 to 64 MiB, in Settings and as `chunk_size_mib` in `config.toml`.
- Fixed: the light theme stayed dark on Linux desktops with a dark GTK theme, such as Ubuntu's.

## v1.0.0

First release.

- Desktop app for Windows, macOS and Linux, in English, Khmer and Arabic, plus the `lanshare` command-line tool.
- Finds devices on the local network with mDNS and UDP broadcast.
- Sends files and folders over QUIC with parallel streams, falling back to TCP.
- Verifies every chunk with BLAKE3, and resumes interrupted transfers where they stopped.
- Encrypts all traffic with TLS 1.3. Devices pair once with a PIN.
- Pause, resume, cancel, a bandwidth limit, and a history of past transfers.
- Light and dark themes, or follow the system.
- Installers with auto-update. The app keeps 7 days of logs, and Settings can export them for bug reports.
- Wire protocol version 6, frozen for the 1.x releases.
