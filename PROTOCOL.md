# LanShare Protocol

Current version: **2** (`PROTOCOL_VERSION` in `crates/lanshare-core/src/protocol/mod.rs`).

Transfers run over plain TCP on port `52000`. One connection carries exactly one file transfer. The connecting side
is the **sender**; the listening side is the **receiver**. Devices find each other with mDNS and a UDP broadcast
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
part of the wire format**. Integers inside messages (`u16`, `u64`) are varint-encoded by `postcard`.

The file contents themselves are **not** framed. See the flow below.

## Messages

| # | Message | Fields | Sent by | Meaning |
|---|---|---|---|---|
| 0 | `Hello` | `protocol_version: u16`, `device_id: DeviceId`, `device_name: String` | sender | First message |
| 1 | `Offer` | `file_name: String`, `size: u64` | sender | Offers one file of exactly `size` bytes |
| 2 | `Accept` | none | receiver | Accepts the offer; raw bytes follow |
| 3 | `Reject` | `code: ErrorCode` | receiver | Refuses the hello or the offer; the connection ends |
| 4 | `Done` | none | sender | All `size` bytes were sent |
| 5 | `Error` | `code: ErrorCode`, `detail: Option<String>` | either | Reports a failure (`detail` is for logs only) |

`DeviceId` is a UUID, encoded by `postcard` as a byte string (length `16`, then the 16 bytes).

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

## Flow

```
Sender                                         Receiver
  |                                               |
  |---- Hello { version, device_id, name } ------>|  version != ours?
  |                                               |    -> Reject { VERSION_MISMATCH }, close
  |---- Offer { file_name, size } --------------->|  unsafe name?
  |                                               |    -> Reject { INVALID_FILE_NAME }, close
  |                                               |  user declines?
  |                                               |    -> Reject { DECLINED }, close
  |                                               |  cannot create file?
  |                                               |    -> Reject { IO }, close
  |<--------------------------------- Accept -----|
  |                                               |
  |==== raw file bytes, exactly `size` bytes ====>|  written to disk as they arrive
  |                                               |
  |---- Done ------------------------------------>|  file complete
  |                                               |
 close                                          close
```

### Receiver rules

- The file name from `Offer` is sanitized: directory components (`/` and `\`) and a leading Windows drive prefix
  (`C:`) are stripped. The result is rejected if it is empty, `.` or `..`, contains control characters, or is longer
  than 255 bytes.
- The file is saved as `<download_dir>/<file_name>`. If that exists, `name (1).ext`, `name (2).ext` and so on are
  tried. Existing files are never overwritten.
- After `Accept`, the receiver reads exactly `size` raw bytes and then expects a `Done` frame. Anything else is a
  `PROTOCOL_VIOLATION`.
- If the connection drops before `Done`, the partial file is deleted. (Resume arrives in a later version.)

### Sender rules

- `Offer.file_name` is the last component of the local path; it must be valid UTF-8.
- The sender sends exactly `size` bytes. If the local file shrinks while sending, the sender stops with `IO`.
- A `Reject` or `Error` in place of `Accept` ends the transfer with that code.
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
