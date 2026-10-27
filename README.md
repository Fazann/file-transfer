# LanShare

Transfer large files between computers on the same local network or Wi-Fi.

> Status: phase 3. One file at a time over plain TCP from the command line, with automatic device discovery, chunk
> hashing and resume. There is **no encryption yet**, so only use it on networks you trust.

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
lanshare send "Office PC" ./movie.mkv
lanshare send 192.168.1.20 ./movie.mkv --port 52000
```

`send` looks the name up on the network, ignoring upper/lower case. If several devices share the name, it lists them
and asks you to use an IP address instead.

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
| `port` | `52000` | TCP port for `receive` |
| `download_dir` | `Downloads/LanShare` | Where received files go |
| `udp_fallback` | `true` | Also discover over UDP broadcast, for networks that block mDNS |
| `stale_part_days` | `7` | Delete unfinished transfers older than this many days when `receive` starts |

### Firewall

Allow these on the receiving computer:

- TCP `52000`: transfers
- UDP `5353`: mDNS discovery
- UDP `52001`: broadcast discovery fallback

**Windows** shows a firewall prompt the first time `lanshare` runs. Choose **Allow** for private networks, or other
devices will not find this one or be able to send to it. (The installer will set this up in a later release.)

Set `RUST_LOG` for logs on stderr, e.g. `RUST_LOG=debug lanshare receive`.

## Project layout

| Folder | What |
|---|---|
| `crates/lanshare-core` | All protocol, network and transfer logic (library `lanshare_core`) |
| `crates/lanshare-cli` | The `lanshare` command-line tool |

The wire protocol is documented in [`PROTOCOL.md`](PROTOCOL.md).

## Development

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
