use ring::digest::{digest, SHA256};

/// GoWay v1.8.4-compatible XOR transform.
/// Each TransformInPlace call starts at key offset zero. The SHA-256 digest
/// of the configured key is repeated to 256 KiB and applied to the buffer.
pub const XOR_KEY_SIZE: usize = 256 * 1024;

#[derive(Clone)]
pub struct XorCipher {
    key: Vec<u8>,
}

impl XorCipher {
    pub fn new(key: &str) -> Self {
        if key.is_empty() {
            return Self { key: Vec::new() };
        }
        let digest = digest(&SHA256, key.as_bytes());
        let digest = digest.as_ref();
        let mut expanded = Vec::with_capacity(XOR_KEY_SIZE);
        while expanded.len() < XOR_KEY_SIZE {
            let take = (XOR_KEY_SIZE - expanded.len()).min(digest.len());
            expanded.extend_from_slice(&digest[..take]);
        }
        Self { key: expanded }
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        !self.key.is_empty()
    }

    /// Applies one GoWay TransformInPlace operation. Offset is reset to zero
    /// for every call, matching the v1.8.4 implementation.
    ///
    /// Bulk path mirrors Go's 8-byte word XOR (`binary.NativeEndian`):
    /// every byte `j` still maps to `key[j % XOR_KEY_SIZE]`, but 8 bytes
    /// are processed per iteration instead of one.
    pub fn apply(&self, data: &mut [u8]) {
        if self.key.is_empty() {
            return;
        }
        let key = &self.key;
        let n = data.len();
        let mut i = 0;
        while i + 8 <= n {
            // `i` is 8-aligned, so `off` is 8-aligned and `off + 8` never
            // exceeds `key.len()` (== XOR_KEY_SIZE).
            let off = i & (XOR_KEY_SIZE - 1);
            let kw = u64::from_ne_bytes(key[off..off + 8].try_into().unwrap());
            let dw = u64::from_ne_bytes(data[i..i + 8].try_into().unwrap());
            data[i..i + 8].copy_from_slice(&(dw ^ kw).to_ne_bytes());
            i += 8;
        }
        while i < n {
            data[i] ^= key[i & (XOR_KEY_SIZE - 1)];
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_is_noop() {
        let cipher = XorCipher::new("");
        let mut data = b"hello rushway".to_vec();
        let original = data.clone();
        cipher.apply(&mut data);
        assert_eq!(data, original);
        assert!(!cipher.is_enabled());
    }

    #[test]
    fn sha256_digest_is_repeated() {
        let cipher = XorCipher::new("test-key");
        assert!(cipher.is_enabled());
        assert_eq!(cipher.key.len(), XOR_KEY_SIZE);
        assert_eq!(&cipher.key[..32], &cipher.key[32..64]);
    }

    #[test]
    fn repeated_transform_round_trips() {
        let plaintext = b"a longer payload that crosses the digest boundary";
        let mut encrypted = plaintext.to_vec();
        let cipher = XorCipher::new("rushway-key");
        cipher.apply(&mut encrypted);
        assert_ne!(encrypted, plaintext);
        cipher.apply(&mut encrypted);
        assert_eq!(encrypted, plaintext);
    }

    fn bytewise_reference(key_digest: &[u8], data: &mut [u8]) {
        for (i, byte) in data.iter_mut().enumerate() {
            *byte ^= key_digest[i % XOR_KEY_SIZE];
        }
    }

    #[test]
    fn bulk_path_matches_bytewise_reference() {
        let cipher = XorCipher::new("bulk-key");
        // Unaligned lengths, digest-boundary crossing, and multi-chunk (>256KiB).
        for len in [1usize, 7, 8, 9, 15, 31, 33, 1000, 65535, 100003, 300 * 1024 + 7] {
            let plain: Vec<u8> = (0..len).map(|i| (i as u64 * 2654435761 % 251 + 1) as u8).collect();
            let mut fast = plain.clone();
            let mut reference = plain.clone();
            cipher.apply(&mut fast);
            // Rebuild the expanded keystream the same way `new` does.
            let digest = ring::digest::digest(&ring::digest::SHA256, b"bulk-key");
            let d = digest.as_ref();
            let mut stream = Vec::with_capacity(XOR_KEY_SIZE);
            while stream.len() < XOR_KEY_SIZE {
                let take = (XOR_KEY_SIZE - stream.len()).min(d.len());
                stream.extend_from_slice(&d[..take]);
            }
            bytewise_reference(&stream, &mut reference);
            assert_eq!(fast, reference, "mismatch at len {len}");
            // Round-trip back to plaintext.
            cipher.apply(&mut fast);
            assert_eq!(fast, plain, "round-trip failed at len {len}");
        }
    }

    #[test]
    fn each_call_resets_offset() {
        let mut whole = b"0123456789abcdef".to_vec();
        let mut first = b"0123456789".to_vec();
        let mut second = b"abcdef".to_vec();
        let cipher = XorCipher::new("offset-key");
        cipher.apply(&mut whole);
        cipher.apply(&mut first);
        cipher.apply(&mut second);
        let mut joined = first;
        joined.extend_from_slice(&second);
        assert_ne!(whole, joined);
    }
}
