use sha2::{Digest, Sha256};

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
        let digest = Sha256::digest(key.as_bytes());
        let mut expanded = Vec::with_capacity(XOR_KEY_SIZE);
        while expanded.len() < XOR_KEY_SIZE {
            let take = (XOR_KEY_SIZE - expanded.len()).min(digest.len());
            expanded.extend_from_slice(&digest[..take]);
        }
        Self { key: expanded }
    }

    pub fn is_enabled(&self) -> bool {
        !self.key.is_empty()
    }

    /// Applies one GoWay TransformInPlace operation. Offset is reset to zero
    /// for every call, matching the v1.8.4 implementation.
    pub fn apply(&self, data: &mut [u8]) {
        if self.key.is_empty() {
            return;
        }
        for (i, byte) in data.iter_mut().enumerate() {
            *byte ^= self.key[i % self.key.len()];
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
