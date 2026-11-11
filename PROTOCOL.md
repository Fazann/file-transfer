# LanShare Protocol

Current version: **8** (`PROTOCOL_VERSION` in `crates/lanshare-core/src/protocol/mod.rs`). Version 7 is version 6
plus [texts](#texts), and version 8 adds [clipboard sync](#clipboard-sync); devices speak all three (see
[Versions](#versions)).

**Version 6 is frozen for the 1.x releases.** Later changes must stay compatible with it, or increase the version
deliberately and be recorded in the [changelog](#changelog) below.

Transfers run over QUIC on UDP port `52000`, or over TCP on port `52000` when QUIC cannot connect (see
[Transports](#transports)). One connection carries one offer or one text. The connecting side is the **sender**; the
listening side is the **receiver**. Files travel in fixed-size chunks, each with its own BLAKE3 hash, over several
parallel data streams, so a dropped transfer can resume and every byte is verified. Every connection is encrypted with
TLS 1.3, and devices recognize each other by certificate fingerprint after pairing with a PIN (see
[Security](#security)). Devices find each other with mDNS and a UDP broadcast fallback, described under
[Discovery](#discovery).

## Transports

Every connection has one **control channel** (both directions) and any number of one-way **data streams** from the
sender to the receiver. Every message except chunk data travels on the control channel, as frames. Data streams carry
only `ChunkHeader` frames, each followed by its chunk's raw bytes, repeated until the sender closes the stream.

| | QUIC (preferred) | TCP (fallback) |
|---|---|---|
| Port | UDP `52000` | TCP `52000` |
| Control channel | the first bidirectional stream, opened by the sender | the first TCP connection |
| Data stream | a unidirectional stream from the sender | one more TCP connection, starting with `DataStreamHello` |
| Encryption | TLS 1.3, ALPN `lanshare` | TLS 1.3 on every TCP connection, control and data |
| Dead peer noticed by | QUIC keep-alive every 5 s, idle timeout 20 s | TCP keep-alive after 5 s idle, every 2 s |

- The sender tries QUIC first. If the QUIC handshake does not finish within **3 seconds**, it connects over TCP
  instead. If neither works, it fails with `TRANSPORT_UNAVAILABLE` (or `PEER_UNREACHABLE` after trying every address
  of a discovered device).
- The receiver listens on both. If the UDP port cannot be opened, it accepts TCP only.
- TCP: after the TLS handshake, the receiver reads the first frame of every new TCP connection. `DataStreamHello`
  attaches the connection to the transfer with that `transfer_id`, but only for the control connection with the same
  certificate fingerprint (transfer ids are not secret); a data connection from any other certificate is dropped.
  Anything else (normally `Hello`) starts a new control channel.
- The receiver keeps at most **8** incoming connections at once (waiting or being handled; TCP data connections do
  not count). It answers the first message of any extra connection with `Reject { BUSY }` and closes it.
- Each TLS handshake, and each read of `Hello` or of the message after it, must finish within **10 seconds**.
- QUIC settings: stream receive window 32 MiB, connection window 64 MiB, BBR congestion control.
- The sender opens `streams` data streams (`config.toml`, default 4, 1 to 16), but never more than there are chunks
  to send. The receiver accepts at most 16 per connection.

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
| 1 | `Offer` | `transfer_id: Uuid`, `chunk_size: u32`, `entries: Vec<OfferEntry>` | sender | Offers files and folders |
| 2 | `Accept` | `resume: Vec<EntryResume>` | receiver | Accepts the offer; empty `resume` for a new transfer |
| 3 | `Reject` | `code: ErrorCode` | receiver | Refuses the hello or the offer; the connection ends |
| 4 | `ChunkHeader` | `entry: u32`, `index: u64`, `len: u32`, `hash: [u8; 32]` | sender | `len` raw bytes follow |
| 5 | `ChunkAck` | `entry: u32`, `index: u64` | receiver | The chunk matched its hash and was written |
| 6 | `ChunkNack` | `entry: u32`, `index: u64` | receiver | The chunk did not match its hash; send it again |
| 7 | `EntryDone` | `entry: u32`, `root_hash: [u8; 32]` | sender | Every chunk of the entry was acknowledged |
| 8 | `Done` | none | both | Sender: everything was sent. Receiver's answer: every entry is verified and saved |
| 9 | `Error` | `code: ErrorCode`, `detail: Option<String>` | either | Reports a failure; the connection ends |
| 10 | `DataStreamHello` | `transfer_id: Uuid` | sender | First frame of an extra TCP data connection |
| 11 | `Pause` | none | either | Stop starting new chunks; chunks on their way finish |
| 12 | `Resume` | none | either | Continue after `Pause` |
| 13 | `BenchOffer` | `transfer_id: Uuid`, `size: u64` | sender | Sent instead of `Offer` by `lanshare bench` |
| 14 | `PairRequest` | none | sender | Sent instead of `Offer` to ask for pairing |
| 15 | `PairMessage` | `data: Vec<u8>` | both | One SPAKE2 message |
| 16 | `PairConfirm` | `mac: [u8; 32]` | both | Proof of the shared key |
| 17 | `PairResult` | `ok: bool` | both | End of pairing |
| 18 | `TextOffer` | `text: String` | sender | Sent instead of `Offer` to hand over a short text (v7) |
| 19 | `ClipboardOffer` | `text: String` | sender | Sent instead of `Offer` to put a text on the receiver's clipboard (v8) |

`Hello` is also sent by the **receiver**, once, as its answer to `PairRequest` (see [Pairing](#pairing)).

`ChunkHeader` is only valid on data streams; every other message only on the control channel.

Structs, with fields in wire order:

| Struct | Fields |
|---|---|
| `OfferEntry` | `relative_path: String` (`/`-separated; a folder ends with `/`), `size: u64` (`0` for folders) |
| `EntryResume` | `entry: u32`, `have: Vec<u8>` (bitset of chunks already received) |

- `DeviceId` and `Uuid` are encoded by `postcard` as a byte string (length `16`, then the 16 bytes).
- `[u8; 32]` is encoded as exactly 32 bytes, with no length prefix.
- `Vec<u8>` and `String` are a varint length followed by the bytes.

### Versions

A device accepts a `Hello` whose `protocol_version` is from **6** (`MIN_PROTOCOL_VERSION`) to **8**
(`PROTOCOL_VERSION`), and answers any other with `Reject { VERSION_MISMATCH }`. A sender announces the **lowest**
version that has every message it will send: `8` before a `ClipboardOffer`, `7` before a `TextOffer`, `6` for files,
benchmarks and pairing, and the receiver's own `Hello` during pairing is `6` too. So a 1.0 device (version 6 only)
still exchanges files and pairs with a newer one, and refuses a text with `VERSION_MISMATCH` instead of failing on a
message it cannot decode; likewise a version 7 device refuses clipboard texts.
Discovery advertises `PROTOCOL_VERSION`, the newest version a device speaks.

`Hello` stays variant `0`, and `protocol_version` stays its first field, in every future version. A receiver that
cannot decode a whole `Hello` still reads that prefix (variant `0`, then a varint `u16`), and if that version is not
one it accepts, it answers `Reject { VERSION_MISMATCH }` instead of `PROTOCOL_VIOLATION`. This works for every peer
from v2 on. A v1 peer put `device_name` first, so v1 and v2 cannot reliably detect each other's version.

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
| 10 | `TransportUnavailable` | `TRANSPORT_UNAVAILABLE` | none (local only: neither QUIC nor TCP connected) |
| 11 | `UntrustedDevice` | `UNTRUSTED_DEVICE` | none (the other device is not paired) |
| 12 | `FingerprintMismatch` | `FINGERPRINT_MISMATCH` | none (a certificate is not the one expected) |
| 13 | `PairingFailed` | `PAIRING_FAILED` | none (wrong PIN, or pairing could not be confirmed) |
| 14 | `PairingLocked` | `PAIRING_LOCKED` | `retry_after_secs: u64` |
| 15 | `Busy` | `BUSY` | none (the receiver has no room for another connection) |
| 16 | `PermissionDenied` | `PERMISSION_DENIED` | `folder: String` (the receiver's folder that refused the write) |
| 17 | `TextTooLong` | `TEXT_TOO_LONG` | `max_bytes: u64` (the most bytes a text may have) |

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
- `transfer_id` is a UUID v5 over the sender's `DeviceId` and, for every entry, its relative path, size, modified time
  and absolute source path. Offering the same unchanged selection again gives the same id; any change gives a new one.

## Flow

Control channel on the left of each arrow, data streams marked `[data]`:

```
Sender                                         Receiver
  |                                               |
  |==== TLS 1.3 handshake, both certificates ====>|  sender checks the receiver's fingerprint (see Security)
  |---- Hello { version, device_id, name } ------>|  version != ours?
  |                                               |    -> Reject { VERSION_MISMATCH }, close
  |---- Offer { transfer_id, chunk_size, -------->|  sender not paired?             -> Reject { UNTRUSTED_DEVICE }
  |             entries: [{ name, size }] }       |  its id paired with another key? -> Reject { FINGERPRINT_MISMATCH }
  |                                               |  too many entries, bad chunk size? -> Reject { PROTOCOL_VIOLATION }
  |             entries: [{ name, size }] }       |  unsafe name?                   -> Reject { INVALID_FILE_NAME }
  |                                               |  not enough free space?         -> Reject { INSUFFICIENT_SPACE }
  |                                               |  new transfer, no auto-accept, user declines? -> Reject { DECLINED }
  |                                               |  download folder not writable?  -> Reject { PERMISSION_DENIED }
  |<-------------- Accept { resume: [have...] } --|  `resume` lists chunks already received
  |                                               |
  |  opens N data streams                         |
  |---- [data] ChunkHeader { e, i, len, hash } -->|
  |==== [data] len raw bytes ====================>|  hash matches?
  |<------------------------ ChunkAck { e, i } ---|    yes: written to disk
  |<----------------------- ChunkNack { e, i } ---|    no: sender sends chunk i again
  |   ... every chunk not in `have`, of every entry, spread over the data streams ...
  |                                               |
  |<---------- Pause / Resume (either side) ----->|
  |                                               |
  |---- EntryDone { e, root_hash } -------------->|  root hash of stored chunk hashes matches?
  |   ... one EntryDone per entry ...             |    no: Error { HASH_MISMATCH }, close
  |---- Done ------------------------------------>|    yes: file renamed to its final name
  |<--------------------------------------- Done -|  sent once every entry is saved
  |                                               |
 close                                          close
```

Missing chunks of all entries wait in one queue, in entry order; each data stream takes the next chunk when it is free, so faster streams carry
more. Each stream may have up to 4 chunks without an answer. Acks and nacks come back on the control channel in the
order the receiver finishes the chunks; a nacked chunk goes back into the queue. The sender sends an entry's `EntryDone` only
after every chunk of that entry is acknowledged, and closes its data streams once nothing is left to send.

After its final message (`Done`, `Reject` or `Error`), the receiver closes its side of the control channel and waits
up to 5 seconds for the sender to close, so the message is not lost when the connection goes away.

### Pause and resume

- Either device may send `Pause` or `Resume` on the control channel at any time during the data phase. The transfer
  is paused while the last message from either side was `Pause`.
- A paused sender starts no new chunks; chunks already being sent finish and are acknowledged as usual.
- A device sends `Pause` or `Resume` only when its own state changes, so a received `Pause` is never echoed back.
- A transfer paused when the connection drops stays paused after the sender reconnects; the sender repeats `Pause`.

### Bandwidth limit

The sender can limit its sending rate to a number of MB/s (1 MB = 1,000,000 bytes), shared by all data streams of a
transfer and changeable while it runs (`bandwidth_limit_mbps` in `config.toml`, `0` = unlimited). It is local to the
sender and not part of the wire format.

### Benchmarks

`BenchOffer { transfer_id, size }` replaces `Offer`. The receiver answers `Accept { resume: [] }` without asking the
user. The sender then sends `size` bytes of generated data as `ChunkHeader` frames (chunks of 8 MiB, `hash` all
zeros) on its data streams, and the receiver discards them without hashing or writing anything. Once all bytes have
arrived the receiver sends `Done`. `transfer_id` is a fresh random UUID, used only to route TCP data connections.

### Texts

`TextOffer { text }` replaces `Offer`, after a `Hello` with version 7:

```
Sender                                         Receiver
  |---- Hello { 7, device_id, name } ----------->|
  |---- TextOffer { text } ---------------------->|  sender not paired?       -> Reject { UNTRUSTED_DEVICE }
  |                                               |  more than 65,536 bytes?  -> Reject { TEXT_TOO_LONG { 65536 } }
  |                                               |  empty?                    -> Reject { PROTOCOL_VIOLATION }
  |<------------------------------------- Done ---|  the text is shown to the user
 close                                          close
```

- `text` is 1 to **65,536** bytes of UTF-8 (`MAX_TEXT_BYTES`). The sender checks this before connecting.
- Only paired devices may send text: the receiver shows it without asking, so `allow_untrusted` does not apply and
  `auto_accept` does not matter. The usual fingerprint checks come first, as for files.
- Nothing is written to disk and nothing is logged but the size. The desktop app keeps the text only until the user
  closes it, and leaves it out of the history and out of notifications.
- There are no data streams, no resume and no retry: if the connection drops, the user sends again.

### Clipboard sync

`ClipboardOffer { text }` replaces `Offer`, after a `Hello` with version 8. It carries a text just copied on the
sender, for the receiver to put on its clipboard:

```
Sender                                         Receiver
  |---- Hello { 8, device_id, name } ----------->|
  |---- ClipboardOffer { text } ----------------->|  sender not paired?               -> Reject { UNTRUSTED_DEVICE }
  |                                               |  sync off for it, or no clipboard? -> Reject { DECLINED }
  |                                               |  too long or empty?               -> as for TextOffer
  |<------------------------------------- Done ---|  the text is on the clipboard
 close                                          close
```

- The rules for `text` are those of `TextOffer`: 1 to 65,536 bytes of UTF-8, never written to disk or logged.
- The receiver takes it only from a paired device whose `sync_clipboard` is on in its own trust store, so both users
  must turn sync on for each other before anything moves. `lanshare receive` has no clipboard and declines every one.
- The desktop app reads its clipboard every 750 ms while some paired device has sync on, and nothing at all
  otherwise. A new text goes to every such device currently found, each over its own connection. Whatever was on the
  clipboard when sync was turned on is not sent, and a text put there by another device is not sent back.
- Syncing is best effort: a device that refuses or cannot be reached is skipped until the next copy.

### Receiver rules

- Every `relative_path` is checked, and the whole offer rejected with `INVALID_FILE_NAME` if any path is absolute, has
  more than 64 components, or has a component that is empty, `.`, `..`, longer than 255 bytes, ends with a dot or a
  space, contains a control character or one of `\ : < > " | ? *`, or is a Windows reserved name (`CON`, `PRN`, `AUX`,
  `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an extension, in any case). Paths are rejected too if two differ
  only by case, or if a file has the name of a folder another entry needs.
- The folder structure is kept under the download folder. Folders that already exist are reused.
- Data is written to `<download_dir>/<path>.lanshare-part`, created at full size when the entry's first chunk arrives.
  Resume state goes to `<path>.lanshare-state` (JSON: `transfer_id`, `entry`, `size`, `chunk_size`, the hex BLAKE3
  hash of every received chunk or `null`, and `saved_as` once the file is saved). Names are tried as `name`,
  `name (1)`, `name (2)` and so on; the first one that holds a state file for this `transfer_id` and entry is resumed,
  otherwise the first one where no final, part or state file exists is used.
- When resuming, the receiver does not ask the user again: the transfer was accepted when it started.
- Before accepting, the receiver checks that the download folder can be written to, and that it has room:
  `needed = total size - bytes already received`.
- `ChunkHeader` must name a file entry, an existing chunk index, and that chunk's exact length; anything else is a
  `PROTOCOL_VIOLATION`.
- A chunk is written at its own offset, so chunks may arrive in any order. Its hash is recorded in the state only
  after the data is synced to disk. The state is saved after every 8 written chunks and whenever the transfer stops.
- A chunk that fails its hash check gets `ChunkNack`; the third failure of the same chunk ends the transfer with
  `Error { HASH_MISMATCH }`.
- On `EntryDone`, every chunk must have been received (otherwise `PROTOCOL_VIOLATION`) and the root hash of the
  stored chunk hashes must match. On success the part file is renamed to the final name (or `name (n)` if that name
  appeared meanwhile; existing files are never overwritten), and the state records `saved_as`, so a reconnect reports
  the file as complete instead of saving it twice. On a mismatch both files are kept and the receiver sends
  `Error { HASH_MISMATCH }`. Folder entries are created at their `EntryDone`, whose `root_hash` is ignored.
- State files are deleted once every entry is saved, just before the final `Done`.
- If the connection drops, the part and state files are kept. On start, the receiver deletes part and state files
  not modified for `stale_part_days` (default 7).
- If no data arrives for 20 seconds during a transfer, and the transfer is not paused, the connection counts as
  lost.
- `DataStreamHello` connections for a `transfer_id` nobody claims are closed once 64 transfers or 16 connections per
  transfer are waiting.

### Sender rules

- Each picked file becomes one entry named after it; each picked folder becomes one entry per file inside it, with
  paths starting at the folder's own name (`Photos/2024/beach.jpg`). A folder with nothing in it is an entry of size
  `0` with a trailing `/`. Symbolic links are skipped. Paths must be valid UTF-8 and pass the checks in
  [Receiver rules](#receiver-rules); an offer holds at most 50,000 entries.
- A chunk never spans two entries. Small files are not grouped: each is its own entry, and the data streams carry
  several of them at once, so per-file overhead stays low.
- The sender sends only the chunks not set in `Accept.resume`. It needs every chunk hash for the root hash, so it
  caches the hashes it computes per `transfer_id` (in the data directory, under `hash-cache/`); a skipped chunk that
  is not in the cache is read and hashed locally. Cache entries are deleted when the transfer completes or after 7
  days.
- A `ChunkNack` makes the sender send that chunk again.
- If the connection drops (or no data moves for 20 seconds while not paused), the sender reconnects after 1 s, 2 s,
  5 s, 10 s, then every 10 s, for up to 2 minutes, and sends the same `Hello` and `Offer`. A connection that delivered
  at least one chunk starts a new 2-minute window when it drops. If no reconnect succeeds, it fails with
  `CONNECTION_LOST`.
- If the local file shrinks while sending, the sender stops with `IO`.
- A `Reject` or `Error` from the receiver ends the transfer with that code.
- When connecting to a discovered peer, the sender tries its addresses in this order: private IPv4, other IPv4, then
  IPv6, waiting up to 3 seconds for each. If none answers, it fails with `PEER_UNREACHABLE`.

## Security

The threat model, and why the design looks like this, are in [`SECURITY.md`](SECURITY.md). This section is the wire
behavior.

### Certificates and fingerprints

- Every device has an Ed25519 key pair and a self-signed X.509 certificate, made on first run and kept in the config
  folder (`device.key`, `device.crt`).
- The **fingerprint** is the SHA-256 hash of the certificate's DER `SubjectPublicKeyInfo` (32 bytes). It is shown as
  the first 12 hex digits in groups, e.g. `A1B2-C3D4-E5F6`, and stored as 64 lowercase hex digits.
- Certificate names, dates and issuers are never checked. A device *is* its key.

### TLS

- Both transports use TLS 1.3 only, with the `ring` crypto provider. QUIC uses ALPN `lanshare`. The client asks for
  server name `lanshare`, which is not checked.
- Both sides must present a certificate. The handshake signature proves each side holds the key for its certificate.
- No session tickets are sent, and sessions are never resumed.
- **The sender checks the receiver during the handshake:**
  - If discovery advertised a fingerprint for the receiver, the certificate must match it, else the handshake fails
    with `FINGERPRINT_MISMATCH`. This applies in pairing mode too.
  - On a normal connection the fingerprint must be in the sender's trust store, else `UNTRUSTED_DEVICE`.
  - On a pairing connection any certificate is accepted, and the sender sends nothing but pairing messages.
- **The receiver checks the sender after the handshake,** because it cannot know before the first messages whether
  the sender wants to pair. After `Hello`:
  - `PairRequest` starts pairing, whoever the sender is (limited by the lockout below).
  - Otherwise the sender's fingerprint must be in the receiver's trust store. If it is not, but the `device_id` from
    `Hello` is paired with a different fingerprint, the answer is `Reject { FINGERPRINT_MISMATCH }`. Any other
    unknown fingerprint gets `Reject { UNTRUSTED_DEVICE }`, unless `allow_untrusted = true` in the receiver's
    `config.toml`, which lets it offer files that the user must accept by hand.
  - A paired device with `auto_accept = true` has its offers accepted without asking. `Accept` itself is unchanged.
- The trust store is re-read before each check, so devices removed by another process are refused at once.

### Pairing

Pairing makes two devices trust each other. One device (the **sender** here, as it opens the connection) connects in
pairing mode; the other (the **receiver**) shows a random 6-digit PIN that the user types on the sender. The PIN
never travels over the network.

```
Sender                                         Receiver
  |---- Hello ----------------------------------->|
  |---- PairRequest ----------------------------->|  locked out?  -> Reject { PAIRING_LOCKED { retry_after_secs } }
  |<------------------------------------- Hello --|  the receiver's own id and name; it shows a new PIN
  |  user types the PIN (receiver waits up to 120 s)
  |---- PairMessage { SPAKE2 message } --------->|
  |<----------------- PairMessage { SPAKE2 message } --|
  |---- PairConfirm { mac_sender } -------------->|  wrong? -> PairResult { ok: false }, close
  |<---------------------- PairConfirm { mac_receiver } --|
  |  wrong? -> PairResult { ok: false }, close    |
  |---- PairResult { ok: true } ----------------->|  adds the sender to its trust store
  |<--------------------- PairResult { ok: true } --|
  |  adds the receiver to its trust store         |
```

- **Key exchange:** SPAKE2 over Ed25519 (the `spake2` crate, symmetric mode). Password: the 6 ASCII digits.
  Identity: the ASCII text `lanshare pairing v1` followed by the 32-byte TLS channel binding.
- **Channel binding:** 32 bytes from the TLS exporter (RFC 5705 / RFC 8446 §7.5) with label
  `EXPORTER-lanshare-channel-binding` and an empty context. Both ends of one TLS session get the same value; someone
  relaying between two TLS sessions gets different values on each side, so the keys differ and pairing fails.
- **Confirmation:** `mac = BLAKE3-keyed(K, role || sender fingerprint || receiver fingerprint)` where
  `K = BLAKE3-derive_key("lanshare pairing v1", SPAKE2 key)`, and `role` is the ASCII text `sender` or `receiver`.
  The fingerprints are the 32 raw bytes; each side knows its own and the other's from TLS. Comparisons are
  constant-time.
- Each side sends `PairResult { ok: true }` only after checking the other's `PairConfirm`, and adds the other device
  (id and name from its `Hello`, fingerprint from TLS) to its trust store with `auto_accept = false`. Pairing again
  replaces any entry with the same id or fingerprint.
- Every message during pairing must arrive within 10 seconds, except the sender's first `PairMessage`, which waits
  for a person and may take up to **120 seconds**.
- **Lockout:** the receiver counts failed pairing attempts per sender IP address (a fingerprint is free to change).
  Any attempt that saw a PIN and did not succeed counts. The third failure within 60 seconds locks that address for
  **5 minutes**; during that time a `PairRequest` is answered with `Reject { PAIRING_LOCKED { retry_after_secs } }`.
  A successful pairing clears the count.
- The pairing connection ends after the `PairResult` messages. A transfer uses a new connection.

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
| `fp` | certificate fingerprint, 64 lowercase hex digits | `a1b2c3...` |

Services with a missing or invalid TXT record are ignored. The advertised fingerprint is only a claim until a TLS
handshake proves it (see [TLS](#tls)). Every 10 seconds each device browses again and asks the
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
| `fingerprint` | `[u8; 32]` | certificate fingerprint, raw bytes |

The peer's address is the datagram's source address. Datagrams without the magic prefix, or that do not decode, are
ignored.

## Changelog

### v8
- New message `ClipboardOffer { text }` (19), with the same limits as `TextOffer`. Receivers take it only from paired
  devices they sync the clipboard with, decline it otherwise, and answer `Done`.
- Receivers accept `Hello` versions 6 to 8. Senders announce 8 only before a `ClipboardOffer`.

### v7
- New message `TextOffer { text }` (18), sent instead of `Offer` to hand over 1 to 65,536 bytes of text. Receivers
  take it from paired devices only and answer `Done`.
- New error code `TEXT_TOO_LONG { max_bytes }` (17).
- Receivers accept `Hello` versions 6 and 7. Senders announce 7 only before a `TextOffer`, so files and pairing work
  with 1.0 devices unchanged.

### v6

- Protocol frozen for the 1.x releases.
- Offers carry several entries: files and folders, with `/`-separated relative paths and strict checks on every path.
  The chunk, `EntryDone`, `Accept.resume` and state formats already held an entry index; `Done` from the receiver now
  means every entry is saved.
- New error code `PERMISSION_DENIED { folder }` (16). Writing to a refused download folder used to be `IO`.

### v5

- TLS 1.3 with a certificate on both sides on every connection, for TCP as well as QUIC. Devices are identified by
  certificate fingerprint; unpaired senders are rejected unless the receiver allows them.
- New messages `PairRequest` (14), `PairMessage` (15), `PairConfirm` (16) and `PairResult` (17). The receiver answers
  `PairRequest` with its own `Hello`.
- New error codes `UNTRUSTED_DEVICE` (11), `FINGERPRINT_MISMATCH` (12), `PAIRING_FAILED` (13),
  `PAIRING_LOCKED { retry_after_secs }` (14) and `BUSY` (15).
- Discovery advertises the fingerprint: TXT record `fp` and a new last field `fingerprint` in the UDP `Announcement`.
- At most 8 incoming connections at once; a 10-second limit on handshakes and setup messages.

### v4

- QUIC (UDP 52000) is the preferred transport, with TCP as the fallback. A connection now has a control channel and
  parallel data streams; `ChunkHeader` frames and chunk bytes travel only on data streams.
- New messages `DataStreamHello` (10), `Pause` (11), `Resume` (12) and `BenchOffer` (13).
- New error code `TRANSPORT_UNAVAILABLE` (10).

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
