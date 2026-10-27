# LanShare Protocol

Current version: **3** (`PROTOCOL_VERSION` in `crates/lanshare-core/src/protocol/mod.rs`).

Transfers run over plain TCP on port `52000`. One connection carries one offer. The connecting side is the
**sender**; the listening side is the **receiver**. Files travel in fixed-size chunks, each with its own BLAKE3 hash,
so a dropped transfer can resume and every byte is verified. Devices find each other with mDNS and a UDP broadcast
fallback, described under [Discovery](#discovery).

## Frame format

Every protocol message is sent as one frame:

```
+----------------------+-------------------------------+
| length: u32 (BE)     | payload: postcard(Message)    |
| 4 bytes              | `length` bytes                |
+----------------------+-------------------------------+
```

- `length` is the payload size in bytes, big-endian.
- `payload` is the [`postcard`](https://docs.rs/postcard) encoding of a `Message`.
- A frame with `length` greater than **1 MiB (1,048,576 bytes)** is rejected with `PROTOCOL_VIOLATION`, without
  reading the payload.
- A payload that does not decode as a `Message` is a `PROTOCOL_VIOLATION`, except a `Hello` from another version
  (see below), which is a `VERSION_MISMATCH`.
- The stream ending in the middle of a frame is `CONNECTION_LOST`.

`postcard` encodes enum variants by their index (a varint), so **the order of `Message` and `ErrorCode` variants is
part of the wire format**. Integers inside messages (`u16`, `u32`, `u64`) are varint-encoded by `postcard`.

Chunk data is **not** framed: each `ChunkHeader` frame is followed by exactly `len` raw bytes. See the flow below.

## Messages

| # | Message | Fields | Sent by | Meaning |
|---|---|---|---|---|
| 0 | `Hello` | `protocol_version: u16`, `device_id: DeviceId`, `device_name: String` | sender | First message |
| 1 | `Offer` | `transfer_id: Uuid`, `chunk_size: u32`, `entries: Vec<OfferEntry>` | sender | Offers one file for now |
| 2 | `Accept` | `resume: Vec<EntryResume>` | receiver | Accepts the offer; empty `resume` for a new transfer |
| 3 | `Reject` | `code: ErrorCode` | receiver | Refuses the hello or the offer; the connection ends |
| 4 | `ChunkHeader` | `entry: u32`, `index: u64`, `len: u32`, `hash: [u8; 32]` | sender | `len` raw bytes follow |
| 5 | `ChunkAck` | `entry: u32`, `index: u64` | receiver | The chunk matched its hash and was written |
| 6 | `ChunkNack` | `entry: u32`, `index: u64` | receiver | The chunk did not match its hash; send it again |
| 7 | `EntryDone` | `entry: u32`, `root_hash: [u8; 32]` | sender | Every chunk of the entry was acknowledged |
| 8 | `Done` | none | both | Sender: everything was sent. Receiver's answer: every entry is verified and saved |
| 9 | `Error` | `code: ErrorCode`, `detail: Option<String>` | either | Reports a failure; the connection ends |

Structs, with fields in wire order:

| Struct | Fields |
|---|---|
| `OfferEntry` | `relative_path: String` (a plain file name for now), `size: u64` |
| `EntryResume` | `entry: u32`, `have: Vec<u8>` (bitset of chunks already received) |

- `DeviceId` and `Uuid` are encoded by `postcard` as a byte string (length `16`, then the 16 bytes).
- `[u8; 32]` is encoded as exactly 32 bytes, with no length prefix.
- `Vec<u8>` and `String` are a varint length followed by the bytes.

`Hello` stays variant `0`, and `protocol_version` stays its first field, in every future version. A receiver that
cannot decode a whole `Hello` still reads that prefix (variant `0`, then a varint `u16`), and if the version differs
from its own it answers `Reject { VERSION_MISMATCH }` instead of `PROTOCOL_VIOLATION`. This works for every peer from
v2 on. A v1 peer put `device_name` first, so v1 and v2 cannot reliably detect each other's version.

`Error.detail` is never shown in the UI.

### Error codes

`ErrorCode` is sent inside `Reject` and `Error`. Codes are never renamed, reordered or removed; new codes are added
at the end.

| # | Code | Name | Data |
|---|---|---|---|
| 0 | `Declined` | `DECLINED` | none |
| 1 | `VersionMismatch` | `VERSION_MISMATCH` | `ours: u16`, `theirs: u16` (as seen by the side sending it) |
| 2 | `InvalidFileName` | `INVALID_FILE_NAME` | none |
| 3 | `Io` | `IO` | none |
| 4 | `ProtocolViolation` | `PROTOCOL_VIOLATION` | none |
| 5 | `ConnectionLost` | `CONNECTION_LOST` | none |
| 6 | `Cancelled` | `CANCELLED` | none |
| 7 | `PeerUnreachable` | `PEER_UNREACHABLE` | none (local only: no address of a discovered peer answered) |
| 8 | `HashMismatch` | `HASH_MISMATCH` | none (a chunk failed 3 times, or the root hash did not match) |
| 9 | `InsufficientSpace` | `INSUFFICIENT_SPACE` | `needed: u64`, `available: u64` (bytes) |

## Chunks and hashes

- A file of `size` bytes is split into `ceil(size / chunk_size)` chunks. Chunk `i` covers bytes
  `i * chunk_size .. min((i + 1) * chunk_size, size)`, so only the last chunk can be shorter. An empty file has no
  chunks.
- The sender offers `chunk_size = 8 MiB` (8,388,608). The receiver accepts any value from 1 byte to 64 MiB, and at
  most 4,194,304 chunks per entry; anything else is rejected with `PROTOCOL_VIOLATION`.
- Each chunk's hash is the BLAKE3 hash of its bytes.
- The **root hash** of an entry is BLAKE3 of all chunk hashes concatenated in index order (`32 * chunk count`
  bytes). For an empty file that is BLAKE3 of zero bytes. It can be computed from the chunk hashes alone, whatever
  order the chunks arrived in.
- `EntryResume.have` holds `ceil(chunk count / 8)` bytes. Chunk `i` is bit `i % 8` (least significant bit first) of
  byte `i / 8`.
- `transfer_id` is a UUID v5 over (sender `DeviceId`, absolute source path, size, modified time). Offering the same
  unchanged file again gives the same id; a changed file gets a new one.

## Flow

```
Sender                                         Receiver
  |                                               |
  |---- Hello { version, device_id, name } ------>|  version != ours?
  |                                               |    -> Reject { VERSION_MISMATCH }, close
  |---- Offer { transfer_id, chunk_size, -------->|  not one entry, bad chunk size? -> Reject { PROTOCOL_VIOLATION }
  |             entries: [{ name, size }] }       |  unsafe name?                   -> Reject { INVALID_FILE_NAME }
  |                                               |  not enough free space?         -> Reject { INSUFFICIENT_SPACE }
  |                                               |  new transfer and user declines? -> Reject { DECLINED }
  |                                               |  cannot create the part file?   -> Reject { IO }
  |<-------------- Accept { resume: [have...] } --|  `resume` lists chunks already received
  |                                               |
  |---- ChunkHeader { 0, i, len, hash } --------->|
  |==== len raw bytes ===========================>|  hash matches?
  |<------------------------ ChunkAck { 0, i } ---|    yes: written to disk
  |<----------------------- ChunkNack { 0, i } ---|    no: sender sends chunk i again
  |          ... every chunk not in `have` ...    |
  |                                               |
  |---- EntryDone { 0, root_hash } -------------->|  root hash of stored chunk hashes matches?
  |---- Done ------------------------------------>|    no: Error { HASH_MISMATCH }, close
  |<--------------------------------------- Done -|    yes: file renamed to its final name
  |                                               |
 close                                          close
```

The sender may send up to 8 chunks before it has an answer for the first; acks and nacks come back in the order the
receiver finishes the chunks. The sender sends `EntryDone` only after every chunk is acknowledged.

### Receiver rules

- The file name from the offer is sanitized: directory components (`/` and `\`) and a leading Windows drive prefix
  (`C:`) are stripped. The result is rejected if it is empty, `.` or `..`, contains control characters, or is longer
  than 255 bytes.
- Data is written to `<download_dir>/<name>.lanshare-part`, created at full size. Resume state goes to
  `<name>.lanshare-state` (JSON: `transfer_id`, `size`, `chunk_size`, and the hex BLAKE3 hash of every received chunk
  or `null`). Names are tried as `name`, `name (1)`, `name (2)` and so on; the first one that holds a state file with
  this `transfer_id` is resumed, otherwise the first one where no final, part or state file exists is used.
- When resuming, the receiver does not ask the user again: the transfer was accepted when it started.
- Free space is checked before accepting: `needed = size - bytes already received`.
- `ChunkHeader` must name entry `0`, an existing chunk index, and that chunk's exact length; anything else is a
  `PROTOCOL_VIOLATION`.
- A chunk is written at its own offset, so chunks may arrive in any order. Its hash is recorded in the state only
  after the data is synced to disk. The state is saved after every 8 written chunks and whenever the transfer stops.
- A chunk that fails its hash check gets `ChunkNack`; the third failure of the same chunk ends the transfer with
  `Error { HASH_MISMATCH }`.
- On `EntryDone`, every chunk must have been received (otherwise `PROTOCOL_VIOLATION`) and the root hash of the
  stored chunk hashes must match. On success the part file is renamed to the final name (or `name (n)` if that name
  appeared meanwhile; existing files are never overwritten) and the state file is deleted. On a mismatch both files
  are kept and the receiver sends `Error { HASH_MISMATCH }`.
- If the connection drops, the part and state files are kept. On start, the receiver deletes part and state files
  not modified for `stale_part_days` (default 7).
- If no data arrives for 20 seconds during a transfer, the connection counts as lost.

### Sender rules

- `OfferEntry.relative_path` is the last component of the local path; it must be valid UTF-8.
- The sender sends only the chunks not set in `Accept.resume`. It needs every chunk hash for the root hash, so it
  caches the hashes it computes per `transfer_id` (in the data directory, under `hash-cache/`); a skipped chunk that
  is not in the cache is read and hashed locally. Cache entries are deleted when the transfer completes or after 7
  days.
- A `ChunkNack` makes the sender send that chunk again.
- If the connection drops (or no data moves for 20 seconds), the sender reconnects after 1 s, 2 s, 5 s, 10 s, then
  every 10 s, for up to 2 minutes, and sends the same `Hello` and `Offer`. A connection that delivered at least one
  chunk starts a new 2-minute window when it drops. If no reconnect succeeds, it fails with `CONNECTION_LOST`.
- If the local file shrinks while sending, the sender stops with `IO`.
- A `Reject` or `Error` from the receiver ends the transfer with that code.
- When connecting to a discovered peer, the sender tries its addresses in this order: private IPv4, other IPv4, then
  IPv6, waiting up to 3 seconds for each. If none answers, it fails with `PEER_UNREACHABLE`.

## Discovery

Every device has a stable `DeviceId` (random UUID v4, stored in `config.toml`). Only devices running
`lanshare receive` (and, later, the app) announce themselves; `lanshare devices` and `lanshare send` only listen.

A device is reported as lost when it says goodbye (mDNS goodbye or a UDP `leaving` announcement), or when it has not
been heard from for **30 seconds**. A device always ignores its own `DeviceId`.

### mDNS

- Service type: `_lanshare._tcp.local.`
- Instance name: the `DeviceId` in hyphenated text form (unique on the network; the display name goes in TXT).
- Host name: `<DeviceId>.local.`, with every interface address of the device.
- SRV port: the TCP transfer port.
- TXT records:

| Key | Value | Example |
|---|---|---|
| `id` | `DeviceId`, hyphenated | `67e55044-10b1-426f-9247-bb680e5fe0c8` |
| `name` | device name, UTF-8, at most 200 bytes | `Office PC` |
| `v` | `PROTOCOL_VERSION` in decimal | `2` |
| `os` | `windows`, `macos` or `linux` | `macos` |

Services with a missing or invalid TXT record are ignored. Every 10 seconds each device browses again and asks the
cache to re-verify known peers (RFC 6762 §10.4), so a peer that vanished without a goodbye is dropped within about
20 seconds.

### UDP broadcast fallback

For networks that block multicast. Enabled by `udp_fallback = true` in `config.toml` (the default).

- Port: UDP `52001`. Sockets use `SO_REUSEADDR` (plus `SO_REUSEPORT` on Unix) so several LanShare processes on one
  machine can listen.
- Every 5 seconds an announcing device sends one datagram to each IPv4 interface's broadcast address (`ip | !netmask`,
  for example `192.168.1.255`), then to `255.255.255.255`. macOS rejects the latter, which is why both are used.
- On shutdown it sends the same datagram once with `leaving = true`.

Datagram layout:

```
+-------------------+--------------------------------+
| magic: "LNSH"     | payload: postcard(Announcement)|
| 4 bytes (ASCII)   | rest of the datagram           |
+-------------------+--------------------------------+
```

`Announcement` fields, in wire order:

| Field | Type | Meaning |
|---|---|---|
| `protocol_version` | `u16` | `PROTOCOL_VERSION`; always first |
| `id` | `DeviceId` | stable device id |
| `name` | `String` | device name |
| `os` | `Os` enum: `0` = Windows, `1` = macOS, `2` = Linux | operating system |
| `port` | `u16` | TCP transfer port |
| `leaving` | `bool` | `true` only in the goodbye datagram |

The peer's address is the datagram's source address. Datagrams without the magic prefix, or that do not decode, are
ignored.

## Changelog

### v3

- Files are sent in hashed chunks. `Offer` is now `Offer { transfer_id, chunk_size, entries }` and `Accept` carries
  `resume`, so interrupted transfers continue where they stopped.
- New messages `ChunkHeader` (4), `ChunkAck` (5), `ChunkNack` (6), `EntryDone` (7); `Done` moved to 8 and `Error`
  to 9. The receiver now answers `Done` once the file is verified and saved.
- Raw bytes after `Accept` are replaced by `ChunkHeader` frames, each followed by its chunk's bytes.
- New error codes `HASH_MISMATCH` (8) and `INSUFFICIENT_SPACE { needed, available }` (9).

### v2

- `Hello` is now `Hello { protocol_version, device_id, device_name }`. `protocol_version` moved to the first field so
  future versions can always read it; `device_id` was added.
- New error code `PEER_UNREACHABLE` (7).
- Added discovery: mDNS service `_lanshare._tcp.local.` with TXT records `id`, `name`, `v`, `os`, and the UDP
  `Announcement` on port 52001.

### v1

- Initial protocol: `Hello`, `Offer`, `Accept`, `Reject`, `Done`, `Error`.
- Length-prefixed `postcard` frames with a 1 MiB limit; unframed raw file bytes after `Accept`.
- Error codes `DECLINED`, `VERSION_MISMATCH`, `INVALID_FILE_NAME`, `IO`, `PROTOCOL_VIOLATION`, `CONNECTION_LOST`,
  `CANCELLED`.
