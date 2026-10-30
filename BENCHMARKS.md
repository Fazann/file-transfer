# Benchmarks

Throughput numbers for LanShare, so changes to the transport can be compared against a baseline.

## How to measure

On the receiving computer:

```sh
lanshare receive --yes
```

On the sending computer, once per row:

```sh
/usr/bin/time -p lanshare bench <device> --size 2GB --transport tcp  --streams 1
/usr/bin/time -p lanshare bench <device> --size 2GB --transport tcp  --streams 4
/usr/bin/time -p lanshare bench <device> --size 2GB --transport tcp  --streams 8
/usr/bin/time -p lanshare bench <device> --size 2GB --transport quic --streams 1
/usr/bin/time -p lanshare bench <device> --size 2GB --transport quic --streams 4
/usr/bin/time -p lanshare bench <device> --size 2GB --transport quic --streams 8
```

- `bench` sends generated data and the receiver throws it away, so neither disk nor hashing is measured.
- MB/s means 1,000,000 bytes per second.
- CPU is the sender's `user + sys` seconds from `/usr/bin/time`, for the whole run (including connecting).
- Use release builds (`cargo build --release`). Run each row at least twice and record the better result.
- Phase 3 only had one TCP stream, so its baseline is the `tcp`, 1 stream row.

## Wi-Fi

Not measured yet. Needs two computers on the same Wi-Fi network.

| Transport | Streams | MB/s | Sender CPU (s) |
|---|---|---|---|
| TCP | 1 | | |
| TCP | 4 | | |
| TCP | 8 | | |
| QUIC | 1 | | |
| QUIC | 4 | | |
| QUIC | 8 | | |

Network: (Wi-Fi standard, band, distance to the router), sender: (machine, OS), receiver: (machine, OS)

## Gigabit Ethernet

Not measured yet. Needs two computers with SSDs on a gigabit switch.

| Transport | Streams | MB/s | Sender CPU (s) |
|---|---|---|---|
| TCP | 1 | | |
| TCP | 4 | | |
| TCP | 8 | | |
| QUIC | 1 | | |
| QUIC | 4 | | |
| QUIC | 8 | | |

Network: (switch, cables), sender: (machine, OS), receiver: (machine, OS)

## Localhost (reference only)

Apple M2 Pro (10 cores), macOS 26.5.1, sender and receiver on the same machine, 2 GB per run, phase 4 release build.
Localhost has no real network limit, so this only shows each transport's CPU ceiling on this machine.

| Transport | Streams | MB/s | Sender CPU (s) |
|---|---|---|---|
| TCP | 1 | 6046 | 0.37 |
| TCP | 4 | 10681 | 0.35 |
| TCP | 8 | 7742 | 0.54 |
| QUIC | 1 | 169 | 15.3 |
| QUIC | 4 | 143 | 19.7 |
| QUIC | 8 | 144 | 19.1 |

QUIC is limited by packet rate here, not by bandwidth: on macOS every 1,452-byte UDP packet is its own system call
(no segmentation offload), about 130,000 packets per second, and almost all the CPU time is `sys`. No packets were
lost. Gigabit Ethernet needs about 85,000 packets per second, so QUIC should still fill it, which the Ethernet table
above has to confirm. Linux and Windows batch UDP packets in the kernel (GSO / USO) and should do much better.

### Phase 5 (TLS on every connection)

Same machine and method, each process with its own identity (separate `HOME`), paired.

| Transport | Streams | MB/s | Sender CPU (s) | vs phase 4 |
|---|---|---|---|---|
| TCP | 1 | 2973 | 0.63 | −51% |
| TCP | 4 | 8046 | 0.90 | −25% |
| TCP | 8 | 8768 | 0.96 | +13% |
| QUIC | 1 | 151 | 18.5 | −11% |
| QUIC | 4 | 142 | 19.7 | −1% |
| QUIC | 8 | 141 | 20.1 | −2% |

TCP now encrypts (AES-GCM), which costs CPU that localhost makes visible, but even one TCP stream stays about 24
times faster than gigabit Ethernet (≈ 118 MB/s), so on a real network the link is the limit, not TLS. QUIC was
already encrypted in phase 4 and is unchanged with the default 4 streams. The phase 5 "drop under 10%" target refers
to the Wi-Fi and Ethernet tables above, which still need two computers.

For comparison, a full phase 3 file transfer (disk, hashing and one TCP stream) of a 2 GiB file over localhost took
about 2 s (≈ 1,050 MiB/s).

## Hashing

`cargo bench -p lanshare-core --bench hashing` (criterion), one 8 MiB chunk on one core:

| Machine | BLAKE3 throughput |
|---|---|
| Apple M2 Pro | 1.83 GiB/s |

Since phase 4, each data stream reads and hashes its own chunks, so with 4 streams the sender hashes on up to 4 cores.
