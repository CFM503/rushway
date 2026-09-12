use sha2::{Digest, Sha256};

/// GoWay v1.8.4-compatible XOR key material.
///
/// A non-empty key is SHA-256 hashed and the 32-byte digest is repeated to
/// form a 256 KiB keystream. The same operation is used for encryption and
/// decryption. An empty key is a no-op.
pub const XOR_KEY_SIZE: usize = 256 * 1024;

#[derive(Clone)]
pub struct XorCipher {
    key: Vec<u8>,
    offset: usize,
}

impl XorCipher {
    pub fn new(key: &str) -> Self {
        if key.is_empty() {
            return Self { key: Vec::new(), offset: 0 };
        }

        let digest = Sha256::digest(key.as_bytes());
        let mut expanded = Vec::with_capacity(XOR_KEY_SIZE);
        while expanded.len() < XOR_KEY_SIZE {
            let take = (XOR_KEY_SIZE - expanded.len()).min(digest.len());
            expanded.extend_from_slice(&digest[..take]);
        }

        Self { key: expanded, offset: 0 }
    }

    pub fn is_enabled(&self) -> bool {
        !self.key.is_empty()
    }

    pub fn apply(&mut self, data: &mut [u8]) {
        if self.key.is_empty() {
            return;
        }

        for byte in data {
            *byte ^= self.key[self.offset];
            self.offset += 1;
            if self.offset == self.key.len() {
                self.offset = 0;
            }
        }
    }

    pub fn reset(&mut self) {
        self.offset = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_is_noop() {
        let mut cipher = XorCipher::new("");
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
    fn xor_round_trip() {
        let plaintext = b"a longer payload that crosses the digest boundary";
        let mut encrypted = plaintext.to_vec();
        let mut enc = XorCipher::new("rushway-key");
        enc.apply(&mut encrypted);
        assert_ne!(encrypted, plaintext);

        let mut dec = XorCipher::new("rushway-key");
        dec.apply(&mut encrypted);
        assert_eq!(encrypted, plaintext);
    }

    #[test]
    fn stream_offset_is_preserved() {
        let plaintext = b"0123456789abcdef";
        let mut whole = plaintext.to_vec();
        let mut split = plaintext.to_vec();
        let mut a = XorCipher::new("offset-key");
        let mut b = XorCipher::new("offset-key");

        a.apply(&mut whole);
        b.apply(&mut split[..5]);
        b.apply(&mut split[5..]);
        assert_eq!(whole, split);
    }
}
