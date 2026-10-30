# Changelog

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
