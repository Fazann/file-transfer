//! Chunk math and hashing: how a file is split into fixed-size chunks, each with its own BLAKE3 hash, and how the
//! root hash of a file is built from those chunk hashes.

use std::{fs::File, io, ops::Range};

use crate::error::{Error, Result};

/// One mebibyte, the unit the chunk size setting uses.
const MIB: u32 = 1024 * 1024;

/// Chunk size the sender offers (8 MiB). It travels in the offer, so it can change without a protocol change.
pub const DEFAULT_CHUNK_SIZE: u32 = DEFAULT_CHUNK_SIZE_MIB * MIB;

/// [`DEFAULT_CHUNK_SIZE`] in MiB, the default of the `chunk_size_mib` setting.
pub const DEFAULT_CHUNK_SIZE_MIB: u32 = 8;

/// Largest chunk size a receiver accepts (64 MiB). Bounds the memory one chunk may take.
pub const MAX_CHUNK_SIZE: u32 = MAX_CHUNK_SIZE_MIB * MIB;

/// [`MAX_CHUNK_SIZE`] in MiB, the largest value of the `chunk_size_mib` setting.
pub const MAX_CHUNK_SIZE_MIB: u32 = 64;

/// Most chunks one entry may have. Keeps the resume bitset (512 KiB) well inside one frame.
pub const MAX_CHUNK_COUNT: u64 = 4 * 1024 * 1024;

/// A BLAKE3 hash, as raw bytes.
pub type ChunkHash = [u8; 32];

/// Number of chunks a file of `size` bytes has. An empty file has none.
///
/// `chunk_size` must not be zero.
#[must_use]
pub fn chunk_count(size: u64, chunk_size: u32) -> u64 {
    size.div_ceil(u64::from(chunk_size))
}

/// How one file is split into chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLayout {
    size: u64,
    chunk_size: u32,
}

impl ChunkLayout {
    /// The layout for a file of `size` bytes.
    ///
    /// # Errors
    ///
    /// [`Error::ProtocolViolation`] if `chunk_size` is zero or above [`MAX_CHUNK_SIZE`], or the file would have more
    /// than [`MAX_CHUNK_COUNT`] chunks.
    pub fn new(size: u64, chunk_size: u32) -> Result<Self> {
        if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
            return Err(Error::protocol_violation(format!("chunk size {chunk_size} is out of range")));
        }
        if chunk_count(size, chunk_size) > MAX_CHUNK_COUNT {
            return Err(Error::protocol_violation(format!("{size} bytes in {chunk_size}-byte chunks is too many")));
        }
        Ok(Self { size, chunk_size })
    }

    /// The layout of an empty file or a folder: no chunks.
    #[must_use]
    pub fn empty(chunk_size: u32) -> Self {
        Self { size: 0, chunk_size }
    }

    /// File size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Size of every chunk except possibly the last.
    #[must_use]
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// Number of chunks.
    #[must_use]
    pub fn count(&self) -> u64 {
        chunk_count(self.size, self.chunk_size)
    }

    /// Byte range of chunk `index` in the file. Only the last chunk can be shorter than the chunk size.
    ///
    /// `index` must be below [`ChunkLayout::count`].
    #[must_use]
    pub fn range(&self, index: u64) -> Range<u64> {
        let start = index * u64::from(self.chunk_size);
        start..(start + u64::from(self.chunk_size)).min(self.size)
    }

    /// Length of chunk `index` in bytes.
    #[must_use]
    pub fn len(&self, index: u64) -> u32 {
        let range = self.range(index);
        // A chunk is never longer than `chunk_size`, which is a `u32`, so this conversion cannot fail.
        u32::try_from(range.end - range.start).unwrap_or(self.chunk_size)
    }
}

/// BLAKE3 hash of one chunk. For multi-megabyte chunks, call this inside `spawn_blocking`.
#[must_use]
pub fn hash_chunk(data: &[u8]) -> ChunkHash {
    *blake3::hash(data).as_bytes()
}

/// Root hash of a file: BLAKE3 of all chunk hashes concatenated in index order.
///
/// Because it only needs the chunk hashes, it can be computed after chunks arrived in any order, without reading the
/// file again.
#[must_use]
pub fn root_hash<'a>(chunk_hashes: impl IntoIterator<Item = &'a ChunkHash>) -> ChunkHash {
    let mut hasher = blake3::Hasher::new();
    for hash in chunk_hashes {
        hasher.update(hash);
    }
    *hasher.finalize().as_bytes()
}

/// Number of bytes in a bitset holding `count` bits.
#[must_use]
pub fn bitset_len(count: u64) -> u64 {
    count.div_ceil(8)
}

/// Builds a bitset where bit `i` is set when `flags` yields `true` at position `i`.
#[must_use]
pub fn bitset_from(count: u64, flags: impl IntoIterator<Item = bool>) -> Vec<u8> {
    let mut bits = vec![0u8; usize::try_from(bitset_len(count)).unwrap_or(0)];
    for (index, flag) in flags.into_iter().enumerate() {
        if flag {
            bits[index / 8] |= 1 << (index % 8);
        }
    }
    bits
}

/// Whether bit `index` is set. Bits past the end of `bits` count as not set.
#[must_use]
pub fn bitset_contains(bits: &[u8], index: u64) -> bool {
    let Ok(byte) = usize::try_from(index / 8) else {
        return false;
    };
    bits.get(byte).is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

/// Reads chunk `index` of `file` into `buffer` (resized to the chunk length) and returns its hash. Blocking.
pub(crate) fn read_and_hash(
    file: &File,
    layout: &ChunkLayout,
    index: u64,
    buffer: &mut Vec<u8>,
) -> io::Result<ChunkHash> {
    let range = layout.range(index);
    buffer.resize(layout.len(index) as usize, 0);
    read_exact_at(file, buffer, range.start)?;
    Ok(hash_chunk(buffer))
}

/// Fills `buffer` from `file` starting at `offset`, without moving a shared file cursor. Blocking.
#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

/// Fills `buffer` from `file` starting at `offset`. Blocking.
///
/// Windows has no `read_exact_at`, so this loops over `seek_read`, which may read fewer bytes than asked.
#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;

    while !buffer.is_empty() {
        match file.seek_read(buffer, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(read) => {
                // `take` moves the slice out so it can be re-sliced without borrowing `buffer` twice.
                buffer = &mut std::mem::take(&mut buffer)[read..];
                offset += read as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// `serde` helpers that store a list of optional chunk hashes as hex strings, e.g. in JSON state files.
///
/// Use with `#[serde(with = "hex_hashes")]` on a `Vec<Option<ChunkHash>>` field. Without it, `serde_json` would
/// write each hash as an array of 32 numbers.
pub(crate) mod hex_hashes {
    use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

    use super::ChunkHash;

    pub(crate) fn serialize<S: Serializer>(hashes: &[Option<ChunkHash>], serializer: S) -> Result<S::Ok, S::Error> {
        let texts: Vec<Option<String>> =
            hashes.iter().map(|hash| hash.map(|hash| blake3::Hash::from_bytes(hash).to_hex().to_string())).collect();
        texts.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Option<ChunkHash>>, D::Error> {
        let texts = Vec::<Option<String>>::deserialize(deserializer)?;
        texts
            .into_iter()
            .map(|text| {
                // `transpose` turns `Option<Result<T>>` into `Result<Option<T>>`, so `collect` can stop at an error.
                text.map(|text| blake3::Hash::from_hex(text).map(|hash| *hash.as_bytes()).map_err(D::Error::custom))
                    .transpose()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn chunk_count_handles_empty_and_exact_multiples() {
        assert_eq!(chunk_count(0, DEFAULT_CHUNK_SIZE), 0);
        assert_eq!(chunk_count(1, DEFAULT_CHUNK_SIZE), 1);
        assert_eq!(chunk_count(8 * MIB, DEFAULT_CHUNK_SIZE), 1);
        assert_eq!(chunk_count(8 * MIB + 1, DEFAULT_CHUNK_SIZE), 2);
        assert_eq!(chunk_count(16 * MIB, DEFAULT_CHUNK_SIZE), 2);
    }

    #[test]
    fn last_chunk_is_shorter() {
        let layout = ChunkLayout::new(20, 8).unwrap();
        assert_eq!(layout.count(), 3);
        assert_eq!(layout.range(0), 0..8);
        assert_eq!(layout.range(2), 16..20);
        assert_eq!(layout.len(2), 4);
    }

    #[test]
    fn exact_multiple_has_full_last_chunk() {
        let layout = ChunkLayout::new(16, 8).unwrap();
        assert_eq!(layout.count(), 2);
        assert_eq!(layout.range(1), 8..16);
        assert_eq!(layout.len(1), 8);
    }

    #[test]
    fn bad_layouts_are_rejected() {
        assert!(ChunkLayout::new(10, 0).is_err());
        assert!(ChunkLayout::new(10, MAX_CHUNK_SIZE + 1).is_err());
        assert!(ChunkLayout::new(MAX_CHUNK_COUNT + 1, 1).is_err());
        assert!(ChunkLayout::new(MAX_CHUNK_COUNT, 1).is_ok());
    }

    #[test]
    fn root_hash_depends_on_index_order_only() {
        let hashes: Vec<ChunkHash> = [b"a".as_slice(), b"b", b"c"].iter().map(|data| hash_chunk(data)).collect();
        let reversed: Vec<ChunkHash> = hashes.iter().rev().copied().collect();
        assert_ne!(root_hash(&hashes), root_hash(&reversed));
        assert_eq!(root_hash(&hashes), root_hash(&hashes.clone()));
        assert_eq!(root_hash(&[]), *blake3::hash(b"").as_bytes());
    }

    #[test]
    fn bitset_round_trips() {
        let flags = [true, false, true, false, false, false, false, false, true, true];
        let bits = bitset_from(10, flags);
        assert_eq!(bits, vec![0b0000_0101, 0b0000_0011]);
        for (index, flag) in flags.iter().enumerate() {
            assert_eq!(bitset_contains(&bits, index as u64), *flag);
        }
        assert!(!bitset_contains(&bits, 100));
        assert_eq!(bitset_from(0, []), Vec::<u8>::new());
    }

    #[test]
    fn chunks_are_read_at_their_offset() {
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"0123456789").unwrap();
        let layout = ChunkLayout::new(10, 4).unwrap();
        let mut buffer = Vec::new();
        let hash = read_and_hash(&file, &layout, 2, &mut buffer).unwrap();
        assert_eq!(buffer, b"89");
        assert_eq!(hash, hash_chunk(b"89"));
    }
}
