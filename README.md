# LanShare

Transfer large files between computers on the same local network or Wi-Fi.

> Status: phase 1. One file at a time over plain TCP from the command line. There is **no encryption yet**, so only
> use it on networks you trust.

## Build

Requires Rust stable (edition 2021).

```sh
cargo build --release
# binary: target/release/lanshare
```

## Usage

On the receiving computer:

```sh
lanshare receive                     # listens on port 52000, saves to ./downloads, asks before accepting
lanshare receive --dir ~/Downloads --yes --port 52000
```

On the sending computer:

```sh
lanshare send 192.168.1.20 ./movie.mkv
lanshare send 192.168.1.20 ./movie.mkv --port 52000
```

If the receiving computer has a firewall, allow incoming TCP on port `52000`.

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
