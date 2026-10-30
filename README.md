# LanShare

Transfer large files between computers on the same local network or Wi-Fi.

LanShare sends files and folders directly between your devices, without a cloud service or an account. It has a desktop
app for Windows, macOS and Linux in English, Khmer and Arabic, and a `lanshare` command-line tool. Devices find each
other automatically, transfers use QUIC with parallel streams (TCP when QUIC is blocked), every chunk is verified, and
an interrupted transfer continues where it stopped. All traffic is encrypted with TLS 1.3, and devices pair once with a
PIN (see [`SECURITY.md`](SECURITY.md)).

## Install

Download the installer for your system from the
[releases page](https://github.com/Fazann/file-transfer/releases/latest). The app updates itself after that.

| System | File | Notes |
|---|---|---|
| Windows | `.exe` (recommended) or `.msi` | The `.exe` asks for administrator rights and adds the firewall rules for you. With the `.msi`, choose **Allow** at the first firewall prompt. |
| macOS | `.dmg` | Choose **Allow** when macOS asks about devices on your local network. |
| Linux | `.AppImage` or `.deb` | Discovery needs Avahi: `sudo apt install avahi-daemon` on Debian and Ubuntu. |

## FAQ

**Another device doesn't appear.** Both devices must be on the same network with LanShare open. Guest Wi-Fi often
blocks devices from seeing each other. Check the [firewall notes](#firewall) too. On macOS, make sure LanShare is on
under System Settings → Privacy & Security → Local Network; if it is off there, macOS blocks LanShare without saying so.

**Do my files go through the internet?** No. They go directly between the two devices, encrypted.

**Is there a size limit?** Files and folders can be any size the receiving disk can hold. One transfer can hold up to
50,000 files and folders.

**The connection dropped. Do I start over?** No. The app reconnects by itself for two minutes. After that, send the
same files again and the transfer continues where it stopped.

**A file with that name already exists.** Nothing is overwritten: the new file is saved as `name (1).ext`.

**How do I report a problem?** Open Settings → Export diagnostics and attach the zip. It holds logs and settings, which
include file names and your device name, but never your device key or paired devices.

## Build

Requires Rust stable (edition 2021).

```sh
cargo build --release
# binary: target/release/lanshare
```

## Usage

On the receiving computer:

```sh
lanshare receive                     # advertises this device, asks before accepting each file
lanshare receive --dir ~/Desktop --yes --port 52000
```

`lanshare receive` saves to `Downloads/LanShare` by default and runs until you press Ctrl+C.

On the sending computer:

```sh
lanshare devices                     # lists devices running `lanshare receive` (listens for 5 seconds)
lanshare pair "Office PC"            # pair once: type the PIN that `lanshare receive` shows
lanshare send "Office PC" ./movie.mkv
lanshare send 192.168.1.20 ./movie.mkv --port 52000
lanshare send "Office PC" ./movie.mkv --streams 8 --limit 10   # 8 parallel streams, at most 10 MB/s
lanshare send "Office PC" ./movie.mkv --transport tcp          # skip QUIC
```

The first time two devices exchange a file they must pair. `send` does this by itself if needed: the receiving
computer shows a 6-digit PIN, and you type it on the sending computer. After that, transfers between the two need no
PIN; the receiver is still asked to accept each file.

```sh
lanshare whoami                      # this device's name, id and fingerprint
lanshare trusted list                # paired devices
lanshare trusted remove "Office PC"  # forget a device; it must pair again
```

Compare fingerprints (`A1B2-C3D4-E5F6`) with `lanshare whoami` on the other device if you want to be sure which
device you are pairing with.

`send` looks the name up on the network, ignoring upper/lower case. If several devices share the name, it lists them
and asks you to use an IP address instead.

`send` connects over QUIC and falls back to TCP if QUIC does not connect within 3 seconds (for example when a
firewall blocks UDP). It prints which one it used.

To measure throughput without touching either disk (see [`BENCHMARKS.md`](BENCHMARKS.md)):

```sh
lanshare bench "Office PC" --size 2GB --streams 4 --transport quic
```

### Interrupted transfers

Every chunk of a file is checked with a BLAKE3 hash, and the whole file is checked again before it gets its final
name. While a file arrives it is stored as `<name>.lanshare-part`, next to a `<name>.lanshare-state` file that records
which parts are complete.

- If the connection drops, `send` reconnects on its own for up to two minutes and continues where it stopped.
- If `send` gives up or is stopped, run the same command again: it prints `Resuming: ... already received` and sends
  only what is missing. The same works after restarting `receive`.
- The receiver checks free space before accepting and refuses files that do not fit.
- Unfinished `.lanshare-part` and `.lanshare-state` files are deleted when `receive` starts, once they are older than
  `stale_part_days`.

### Configuration

The first run creates `config.toml`, shared by the CLI and (later) the desktop app:

| OS | Location |
|---|---|
| macOS | `~/Library/Application Support/com.LanShare.LanShare/config.toml` |
| Linux | `~/.config/lanshare/config.toml` |
| Windows | `%APPDATA%\LanShare\LanShare\config\config.toml` |

| Key | Default | Meaning |
|---|---|---|
| `device_id` | random, generated once | Identifies this device. Do not copy it to another machine. |
| `device_name` | the computer's host name | Name other devices see |
| `port` | `52000` | Port for `receive`, used for both TCP and UDP (QUIC) |
| `download_dir` | `Downloads/LanShare` | Where received files go |
| `udp_fallback` | `true` | Also discover over UDP broadcast, for networks that block mDNS |
| `stale_part_days` | `7` | Delete unfinished transfers older than this many days when `receive` starts |
| `streams` | `4` | Parallel data streams per outgoing transfer, 1 to 16 |
| `bandwidth_limit_mbps` | `0` | Outgoing limit in MB/s (1 MB = 1,000,000 bytes); `0` means unlimited |
| `allow_untrusted` | `false` | Let unpaired devices offer files (you are still asked). Traffic is encrypted either way |

The same folder holds `device.key` and `device.crt` (this device's identity; keep `device.key` private and never copy
it to another machine) and `trusted_devices.toml` (paired devices). To accept a paired device's files without being
asked, set `auto_accept = true` on its entry in `trusted_devices.toml`.

### Firewall

Allow these on the receiving computer:

- UDP `52000`: transfers over QUIC
- TCP `52000`: transfers over TCP, when QUIC is blocked
- UDP `5353`: mDNS discovery
- UDP `52001`: broadcast discovery fallback

**Windows:** the `.exe` installer adds rules for TCP `52000`, UDP `52000` and UDP `52001`, on all network profiles, and
the uninstaller removes them. With the `.msi` or the command-line tool, Windows shows a firewall prompt the first time
LanShare runs. Choose **Allow**, or other devices will not find this one or be able to send to it.

Set `RUST_LOG` for logs on stderr, e.g. `RUST_LOG=debug lanshare receive`. The app also keeps 7 days of daily logs in
its data folder.

## Project layout

| Folder | What |
|---|---|
| `crates/lanshare-core` | All protocol, network and transfer logic (library `lanshare_core`) |
| `crates/lanshare-cli` | The `lanshare` command-line tool |
| `app` | The desktop app: Tauri 2 (`app/src-tauri`, crate `lanshare-app`) with a Svelte 5 frontend (`app/src`) |
| `fuzz` | `cargo-fuzz` targets (not a workspace member) |

The wire protocol is documented in [`PROTOCOL.md`](PROTOCOL.md), the security design in [`SECURITY.md`](SECURITY.md).

## Releasing

1. Set the version in `Cargo.toml`, `app/package.json` and `app/src-tauri/tauri.conf.json`, and update `CHANGELOG.md`.
2. Push a tag such as `v1.0.1`. `release.yml` builds every installer into a draft release.
3. Check the draft, then publish it. Publishing is what offers the update to users.

`release.yml` needs the repository secrets `TAURI_SIGNING_PRIVATE_KEY` (the contents of the updater private key) and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (may be empty). The public key is in `tauri.conf.json`. Keep the private key safe:
installed apps only accept updates signed with it. macOS builds are signed and notarized when the `APPLE_*` secrets
listed in `release.yml` exist. A local `tauri build` needs `TAURI_SIGNING_PRIVATE_KEY` set as well.

## Development

`make` lists the common commands: `make dev` runs the app with live reload, `make check` runs what CI runs, and
`make release` builds installers for the computer you are on (`release-mac`, `release-linux` for Debian and Ubuntu,
`release-windows`). Installers can only be built on their own system. On Windows, `make` needs Git Bash. On Debian and
Ubuntu, run `make deps-linux` once first.

Formatting uses nightly-only rustfmt options, so install the nightly rustfmt once:

```sh
rustup toolchain install nightly --profile minimal -c rustfmt
```

Checks that must pass before committing:

```sh
cargo +nightly fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

The desktop app needs Node.js 20 or newer. From `app/`:

```sh
npm ci
npm run tauri dev       # run the app with live reload
npm run tauri build     # build an installer for this OS
```

Frontend checks (from `app/`):

```sh
npm run format:check && npm run lint && npm run check && npm run test
```

The app and the CLI share one identity, trust list and `config.toml`, so a device paired in one is paired in the
other. App-only settings (language, close to tray, start at login) are in `app-settings.json` in the same folder, and
the transfer history in `history.db` in the data folder.

Fuzzing the frame decoder needs `cargo install cargo-fuzz` and the nightly toolchain:

```sh
cd fuzz && cargo +nightly fuzz run read_message -- -max_total_time=600
```
