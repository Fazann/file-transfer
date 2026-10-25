# LanShare Protocol

Current version: **1** (`PROTOCOL_VERSION` in `crates/lanshare-core/src/protocol/mod.rs`).

Phase 1 runs over plain TCP on port `52000`. One connection carries exactly one file transfer. The connecting side
is the **sender**; the listening side is the **receiver**.

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
- A payload that does not decode as a `Message` is a `PROTOCOL_VIOLATION`.
- The stream ending in the middle of a frame is `CONNECTION_LOST`.

`postcard` encodes enum variants by their index (a varint), so **the order of `Message` and `ErrorCode` variants is
part of the wire format**. Integers inside messages (`u16`, `u64`) are varint-encoded by `postcard`.

The file contents themselves are **not** framed. See the flow below.

## Messages

| # | Message | Fields | Sent by | Meaning |
|---|---|---|---|---|
| 0 | `Hello` | `device_name: String`, `protocol_version: u16` | sender | First message on every connection |
| 1 | `Offer` | `file_name: String`, `size: u64` | sender | Offers one file of exactly `size` bytes |
| 2 | `Accept` | none | receiver | Accepts the offer; raw bytes follow |
| 3 | `Reject` | `code: ErrorCode` | receiver | Refuses the hello or the offer; the connection ends |
| 4 | `Done` | none | sender | All `size` bytes were sent |
| 5 | `Error` | `code: ErrorCode`, `detail: Option<String>` | either | Reports a failure (`detail` is for logs only) |

`Hello` must remain variant `0` with the same two fields in every future version, so that peers on any version can
decode it and answer with `VERSION_MISMATCH`.

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

## Flow

```
Sender                                         Receiver
  |                                               |
  |---- Hello { device_name, protocol_version } ->|  version != ours?
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

## Changelog

### v1

- Initial protocol: `Hello`, `Offer`, `Accept`, `Reject`, `Done`, `Error`.
- Length-prefixed `postcard` frames with a 1 MiB limit; unframed raw file bytes after `Accept`.
- Error codes `DECLINED`, `VERSION_MISMATCH`, `INVALID_FILE_NAME`, `IO`, `PROTOCOL_VIOLATION`, `CONNECTION_LOST`,
  `CANCELLED`.
