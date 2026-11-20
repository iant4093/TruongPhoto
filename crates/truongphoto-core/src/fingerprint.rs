use crate::error::CoreError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Number of bytes sampled from the start and end of a file for partial BLAKE3 hashing.
pub const PARTIAL_BLAKE3_CHUNK_BYTES: usize = 64 * 1024;

/// Computes the partial BLAKE3 hex digest from a seekable reader:
/// `BLAKE3(first 64KB || last 64KB || le_u64(file_size))`.
///
/// For files `<= 64 KB`, the entire file is hashed once followed by `le_u64(file_size)`.
/// For files `> 64 KB`, the first 64 KB is hashed, then the last 64 KB (starting at
/// `file_size - 64 KB`), followed by `le_u64(file_size)`.
pub fn partial_blake3_reader<R: Read + Seek>(reader: &mut R) -> Result<String, CoreError> {
    let file_size = reader.seek(SeekFrom::End(0))?;
    let mut hasher = blake3::Hasher::new();

    reader.seek(SeekFrom::Start(0))?;
    let first_len = usize::try_from(file_size.min(PARTIAL_BLAKE3_CHUNK_BYTES as u64))
        .unwrap_or(PARTIAL_BLAKE3_CHUNK_BYTES);
    if first_len > 0 {
        let mut first_buf = vec![0u8; first_len];
        reader.read_exact(&mut first_buf)?;
        hasher.update(&first_buf);
    }

    if file_size > PARTIAL_BLAKE3_CHUNK_BYTES as u64 {
        let tail_offset = file_size - PARTIAL_BLAKE3_CHUNK_BYTES as u64;
        reader.seek(SeekFrom::Start(tail_offset))?;
        let mut last_buf = vec![0u8; PARTIAL_BLAKE3_CHUNK_BYTES];
        reader.read_exact(&mut last_buf)?;
        hasher.update(&last_buf);
    }

    hasher.update(&file_size.to_le_bytes());
    Ok(hasher.finalize().to_hex().to_string())
}

/// Computes `partial_blake3_reader` for the file at `path`.
pub fn partial_blake3_path(path: &Path) -> Result<String, CoreError> {
    let mut file = File::open(path)?;
    partial_blake3_reader(&mut file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn make_pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    #[test]
    fn partial_blake3_small_files_are_deterministic_and_sensitive() {
        let empty = partial_blake3_reader(&mut Cursor::new(Vec::<u8>::new())).unwrap();
        assert_eq!(empty.len(), 64);
        assert_eq!(
            empty,
            partial_blake3_reader(&mut Cursor::new(Vec::<u8>::new())).unwrap()
        );

        let small = make_pattern(1024);
        let h1 = partial_blake3_reader(&mut Cursor::new(&small)).unwrap();
        let h2 = partial_blake3_reader(&mut Cursor::new(&small)).unwrap();
        assert_eq!(h1, h2);
        assert_ne!(h1, empty);

        let mut mutated = small.clone();
        mutated[512] ^= 0x01;
        let h_mut = partial_blake3_reader(&mut Cursor::new(&mutated)).unwrap();
        assert_ne!(h1, h_mut);
    }

    #[test]
    fn partial_blake3_medium_128kb_and_exact_64kb_boundaries() {
        let exact_64k = make_pattern(PARTIAL_BLAKE3_CHUNK_BYTES);
        let h_64k = partial_blake3_reader(&mut Cursor::new(&exact_64k)).unwrap();

        let mut exact_64k_plus_1 = exact_64k.clone();
        exact_64k_plus_1.push(0);
        let h_64k1 = partial_blake3_reader(&mut Cursor::new(&exact_64k_plus_1)).unwrap();
        assert_ne!(h_64k, h_64k1);

        let medium_128k = make_pattern(128 * 1024);
        let h_med = partial_blake3_reader(&mut Cursor::new(&medium_128k)).unwrap();
        assert_eq!(
            h_med,
            partial_blake3_reader(&mut Cursor::new(&medium_128k)).unwrap()
        );

        let mut mut_first = medium_128k.clone();
        mut_first[100] ^= 0x80;
        assert_ne!(
            h_med,
            partial_blake3_reader(&mut Cursor::new(&mut_first)).unwrap()
        );

        let mut mut_last = medium_128k.clone();
        mut_last[128 * 1024 - 1] ^= 0x01;
        assert_ne!(
            h_med,
            partial_blake3_reader(&mut Cursor::new(&mut_last)).unwrap()
        );
    }

    #[test]
    fn partial_blake3_large_256kb_detects_first_last_and_size_mutations() {
        let large = make_pattern(256 * 1024 + 33);
        let baseline = partial_blake3_reader(&mut Cursor::new(&large)).unwrap();

        // 1-byte mutation in first 64KB
        let mut first_mut = large.clone();
        first_mut[65_535] ^= 0x01;
        assert_ne!(
            baseline,
            partial_blake3_reader(&mut Cursor::new(&first_mut)).unwrap()
        );

        // 1-byte mutation in last 64KB
        let mut last_mut = large.clone();
        let start_of_tail = last_mut.len() - PARTIAL_BLAKE3_CHUNK_BYTES;
        last_mut[start_of_tail] ^= 0x01;
        assert_ne!(
            baseline,
            partial_blake3_reader(&mut Cursor::new(&last_mut)).unwrap()
        );

        // Size mutation (1 extra byte inserted in middle)
        let mut size_mut = large.clone();
        size_mut.insert(128 * 1024, 0x42);
        assert_ne!(
            baseline,
            partial_blake3_reader(&mut Cursor::new(&size_mut)).unwrap()
        );
    }

    #[test]
    fn partial_blake3_path_matches_reader() {
        let dir = std::env::temp_dir();
        let file_path = dir.join(format!("tp-blake3-test-{}.bin", std::process::id()));
        let data = make_pattern(192 * 1024);
        {
            let mut f = File::create(&file_path).unwrap();
            f.write_all(&data).unwrap();
        }
        let from_path = partial_blake3_path(&file_path).unwrap();
        let from_reader = partial_blake3_reader(&mut Cursor::new(&data)).unwrap();
        let _ = std::fs::remove_file(&file_path);
        assert_eq!(from_path, from_reader);
    }
}
