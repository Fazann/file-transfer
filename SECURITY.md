# Security

How LanShare protects transfers, what it protects against, and what it does not. The exact wire format is in
[`PROTOCOL.md`](PROTOCOL.md#security).

## Threat model

LanShare is used on shared networks: home Wi-Fi, but also cafés, hotels and offices. Anyone on the same network may
watch, change, or inject traffic.

**Protected against.** Someone on the same network must not be able to:

- read what is transferred, including file names, file contents and device names;
- send files to a device without its user's consent;
- pretend to be a device the user already paired with, even by copying its name and device id;
- slip themselves between two devices while they pair (a "man in the middle");
- guess the pairing PIN by trying many times.

**Not protected against:**

- Someone with access to the user's own computer or account. They can read `device.key`, edit
  `trusted_devices.toml`, or read received files directly.
- Seeing *that* two devices talk, how much data they move and when. Discovery announcements (device name, id, operating
  system and fingerprint) are sent in the clear, so every device on the network can see who runs LanShare.
- Denial of service: someone on the network can block traffic or keep a receiver busy. The limits below only make
  this harder.
- A user who reads a PIN aloud to the wrong person, or turns on `allow_untrusted` and accepts a file from anyone.

## Device identity

On first run each device creates an **Ed25519 key pair** and a self-signed certificate:

| File | Contents |
|---|---|
| `device.key` | The private key (PEM). Readable only by its owner (`0600`) on macOS and Linux. Never leaves the device. |
| `device.crt` | The certificate, which carries the public key (PEM). |

Both live in the config folder (see the README). A device is recognized by its **fingerprint**: the SHA-256 hash of
its public key. People compare the short form, the first 12 hex digits in groups (`lanshare whoami` shows it, e.g.
`A1B2-C3D4-E5F6`). The device id and name are only labels; anyone can copy them, and nothing trusts them.

Deleting `device.key` gives the device a new identity, and every other device must pair with it again.

## Encryption and authentication

Every connection uses **TLS 1.3** with a certificate on **both** sides, over QUIC and over the TCP fallback alike.
Each data stream over TCP is its own TLS connection. The TLS handshake proves that each side holds the private key
for the certificate it shows; a copied certificate is useless without the key.

Certificates are self-signed, so no certificate authority is involved and names and dates are not checked. Instead:

- **The sending device checks the receiver during the handshake.** If discovery advertised a fingerprint, the
  receiver's certificate must match it, or the connection fails with `FINGERPRINT_MISMATCH`. The fingerprint must also
  be in the sender's trust store, or the connection fails with `UNTRUSTED_DEVICE` (and `lanshare send` offers to
  pair). Nothing is sent before these checks pass, so an unknown device never sees a file name.
- **The receiving device checks the sender right after the handshake,** before it reads an offer: the sender's
  fingerprint must be in the receiver's trust store. A sender reusing a paired device's id with a different key gets
  `FINGERPRINT_MISMATCH`; any other unknown sender gets `UNTRUSTED_DEVICE`. The receiver cannot make this decision
  inside the TLS handshake because only the first message tells it whether the sender wants to pair, so its TLS
  verifier only checks that the sender holds its key, and the trust decision follows immediately.
- Data connections over TCP are only attached to a transfer if they come from the same certificate as the control
  connection, so a third device that learns a transfer id cannot add data to it.

## Trust store

Paired devices are listed in `trusted_devices.toml` in the config folder, shared by the CLI and the app:

```toml
[[devices]]
id = "67e55044-10b1-426f-9247-bb680e5fe0c8"
name = "Office PC"
fingerprint = "a1b2c3d4e5f6..."   # 64 hex digits
paired_at = 1793145600            # seconds since 1970-01-01 UTC
auto_accept = false
```

- Only the fingerprint matters for trust; `id` and `name` are what the device reported when it paired.
- **Trust policy on the receiver:** a paired device with `auto_accept = true` sends without a prompt; a paired device
  without it needs the user to accept each file; an unpaired device is rejected with `UNTRUSTED_DEVICE`, unless
  `allow_untrusted = true` in `config.toml`, in which case the user is asked and the prompt says the device is not
  paired. `auto_accept` and `allow_untrusted` are both off by default.
- `lanshare trusted remove <device>` forgets a device. A running receiver re-reads the file before every check, so the
  device is refused at once and must pair again.

## Pairing

Pairing turns two unknown devices into trusted ones. One device asks (`lanshare pair <device>`, or `lanshare send`
when needed); the other shows a random **6-digit PIN**, and the user types it on the first device.

The PIN is never sent. Both devices run **SPAKE2**, a password-authenticated key exchange, with the PIN as the
password. Each then proves it derived the same key by sending a confirmation code (a keyed BLAKE3 hash that also covers
both fingerprints and its role). Only if both confirmations check out does each device add the other to its trust
store.

Why this is safe:

- **Eavesdropping** on a pairing reveals nothing about the PIN: SPAKE2 messages are random-looking, and the
  confirmations only show whether two keys match. An attacker cannot test PIN guesses offline.
- **Guessing online** gives one guess per attempt, so one chance in a million. After 3 failed attempts within a
  minute from one IP address, the receiver refuses pairing from that address for 5 minutes (`PAIRING_LOCKED`).
  Failures are counted per address, not per certificate, because certificates cost nothing to make.
- **A man in the middle** would have to run two TLS sessions, one with each device. SPAKE2 is bound to the TLS
  session through a TLS exporter value (channel binding), which differs between those two sessions, so the two
  devices derive different keys and pairing fails even if the attacker knows the PIN.
- A pairing waits at most 120 seconds for the PIN to be typed; every other step must finish within 10 seconds.

For extra certainty, users can compare the fingerprints shown during pairing with `lanshare whoami` on each device.

## Hardening

- A receiver keeps at most 8 incoming connections at once; extra ones are answered with `BUSY` and closed.
- Each TLS handshake, and each setup or pairing message, has a 10-second time limit (except PIN entry).
- Frames are limited to 1 MiB and checked before they are decoded. The frame decoder (`protocol::read_message`) is
  fuzzed with `cargo-fuzz` (see the README).
- File names in offers are sanitized, and received files never overwrite existing ones.
- TLS 1.3 only, through `rustls` with the `ring` crypto provider. No session resumption.

## Paths and updates

- Every path in an offer is checked before anything is written: absolute paths, `..`, drive letters, reserved Windows
  names and unsafe characters reject the whole offer, so a peer can never write outside the download folder. Existing
  files are never overwritten.
- `PERMISSION_DENIED` tells the sender the path of the receiver's download folder. Only senders that passed the trust
  policy can get this far.
- App updates are signed with a private key kept outside the repository. The app installs only what its built-in public
  key verifies.
- Exported diagnostics hold logs (with file and device names), `config.toml` and `app-settings.json`. Files are added
  from a fixed list, so `device.key` and `trusted_devices.toml` are never included.

## Reporting a problem

Please report security problems privately to the maintainer instead of opening a public issue.
