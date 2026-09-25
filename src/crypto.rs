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

    /// Raw repeated keystream (`key[j]` == stream byte for data offset `j`).
    /// Exposed for fused single-pass transforms (e.g. cipher + WS mask in
    /// one loop); same-crate use only.
    pub(crate) fn keystream(&self) -> &[u8] {
        &self.key
    }

    /// Applies one GoWay TransformInPlace operation. Offset is reset to zero
    /// for every call, matching the v1.8.4 implementation.
    ///
    /// Runtime dynamic dispatch: detects CPU capabilities at runtime.
    /// - AVX2 path: 256-bit YMM registers, 4-way unrolled (128 bytes/iter).
    /// - SSE2 path: 128-bit XMM registers, 4-way unrolled (64 bytes/iter).
    /// - Fallback: 32-byte chunks and scalar tail.
    pub fn apply(&self, data: &mut [u8]) {
        if self.key.is_empty() || data.is_empty() {
            return;
        }
        let key = &self.key;
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2() {
                for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                    let clen = chunk.len();
                    unsafe {
                        apply_chunk_avx2(chunk, &key[..clen]);
                    }
                }
                return;
            } else {
                for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                    let clen = chunk.len();
                    unsafe {
                        apply_chunk_sse2(chunk, &key[..clen]);
                    }
                }
                return;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                let clen = chunk.len();
                unsafe {
                    apply_chunk_neon(chunk, &key[..clen]);
                }
            }
            return;
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                let clen = chunk.len();
                apply_chunk_fallback(chunk, &key[..clen]);
            }
        }
    }
}

#[inline(always)]
pub(crate) fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn apply_chunk_avx2(chunk: &mut [u8], key: &[u8]) {
    use std::arch::x86_64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 128 <= len {
        let k0 = _mm256_loadu_si256(key.as_ptr().add(i) as *const __m256i);
        let k1 = _mm256_loadu_si256(key.as_ptr().add(i + 32) as *const __m256i);
        let k2 = _mm256_loadu_si256(key.as_ptr().add(i + 64) as *const __m256i);
        let k3 = _mm256_loadu_si256(key.as_ptr().add(i + 96) as *const __m256i);

        let d0 = _mm256_loadu_si256(chunk.as_ptr().add(i) as *const __m256i);
        let d1 = _mm256_loadu_si256(chunk.as_ptr().add(i + 32) as *const __m256i);
        let d2 = _mm256_loadu_si256(chunk.as_ptr().add(i + 64) as *const __m256i);
        let d3 = _mm256_loadu_si256(chunk.as_ptr().add(i + 96) as *const __m256i);

        _mm256_storeu_si256(chunk.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d0, k0));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 32) as *mut __m256i, _mm256_xor_si256(d1, k1));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 64) as *mut __m256i, _mm256_xor_si256(d2, k2));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 96) as *mut __m256i, _mm256_xor_si256(d3, k3));
        i += 128;
    }
    while i + 32 <= len {
        let k = _mm256_loadu_si256(key.as_ptr().add(i) as *const __m256i);
        let d = _mm256_loadu_si256(chunk.as_ptr().add(i) as *const __m256i);
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d, k));
        i += 32;
    }
    while i + 16 <= len {
        let k = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub(crate) unsafe fn apply_chunk_sse2(chunk: &mut [u8], key: &[u8]) {
    use std::arch::x86_64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 64 <= len {
        let k0 = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let k1 = _mm_loadu_si128(key.as_ptr().add(i + 16) as *const __m128i);
        let k2 = _mm_loadu_si128(key.as_ptr().add(i + 32) as *const __m128i);
        let k3 = _mm_loadu_si128(key.as_ptr().add(i + 48) as *const __m128i);

        let d0 = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        let d1 = _mm_loadu_si128(chunk.as_ptr().add(i + 16) as *const __m128i);
        let d2 = _mm_loadu_si128(chunk.as_ptr().add(i + 32) as *const __m128i);
        let d3 = _mm_loadu_si128(chunk.as_ptr().add(i + 48) as *const __m128i);

        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d0, k0));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 16) as *mut __m128i, _mm_xor_si128(d1, k1));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 32) as *mut __m128i, _mm_xor_si128(d2, k2));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 48) as *mut __m128i, _mm_xor_si128(d3, k3));
        i += 64;
    }
    while i + 16 <= len {
        let k = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) unsafe fn apply_chunk_neon(chunk: &mut [u8], key: &[u8]) {
    use std::arch::aarch64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 64 <= len {
        let k0 = vld1q_u8(key.as_ptr().add(i));
        let k1 = vld1q_u8(key.as_ptr().add(i + 16));
        let k2 = vld1q_u8(key.as_ptr().add(i + 32));
        let k3 = vld1q_u8(key.as_ptr().add(i + 48));

        let d0 = vld1q_u8(chunk.as_ptr().add(i));
        let d1 = vld1q_u8(chunk.as_ptr().add(i + 16));
        let d2 = vld1q_u8(chunk.as_ptr().add(i + 32));
        let d3 = vld1q_u8(chunk.as_ptr().add(i + 48));

        vst1q_u8(chunk.as_mut_ptr().add(i), veorq_u8(d0, k0));
        vst1q_u8(chunk.as_mut_ptr().add(i + 16), veorq_u8(d1, k1));
        vst1q_u8(chunk.as_mut_ptr().add(i + 32), veorq_u8(d2, k2));
        vst1q_u8(chunk.as_mut_ptr().add(i + 48), veorq_u8(d3, k3));
        i += 64;
    }
    while i + 16 <= len {
        let k = vld1q_u8(key.as_ptr().add(i));
        let d = vld1q_u8(chunk.as_ptr().add(i));
        vst1q_u8(chunk.as_mut_ptr().add(i), veorq_u8(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

pub(crate) fn apply_chunk_fallback(chunk: &mut [u8], key: &[u8]) {
    let clen = chunk.len();
    let (dst_chunks, dst_tail) = chunk.as_chunks_mut::<32>();
    let (src_chunks, src_tail) = key[..clen].as_chunks::<32>();
    for (d, s) in dst_chunks.iter_mut().zip(src_chunks) {
        for b in 0..32 {
            d[b] ^= s[b];
        }
    }
    for (d, s) in dst_tail.iter_mut().zip(src_tail) {
        *d ^= *s;
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
        for len in [
            1usize,
            7,
            8,
            9,
            15,
            31,
            33,
            1000,
            65535,
            100003,
            300 * 1024 + 7,
        ] {
            let plain: Vec<u8> = (0..len)
                .map(|i| (i as u64 * 2654435761 % 251 + 1) as u8)
                .collect();
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

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_sse2_and_avx2_paths_match_reference() {
        let key: Vec<u8> = (0..2048).map(|x| (x * 13 + 7) as u8).collect();
        for len in [
            0usize, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256,
            1000, 2048,
        ] {
            let orig: Vec<u8> = (0..len).map(|x| (x * 7 + 3) as u8).collect();
            let mut ref_buf = orig.clone();
            for (i, b) in ref_buf.iter_mut().enumerate() {
                *b ^= key[i % key.len()];
            }
            let key_expanded: Vec<u8> = (0..len).map(|i| key[i % key.len()]).collect();

            // Test SSE2 path directly.
            let mut sse_buf = orig.clone();
            unsafe {
                apply_chunk_sse2(&mut sse_buf, &key_expanded);
            }
            assert_eq!(sse_buf, ref_buf, "SSE2 mismatch at len {len}");

            // Test AVX2 path directly if supported by host CPU.
            if has_avx2() {
                let mut avx_buf = orig.clone();
                unsafe {
                    apply_chunk_avx2(&mut avx_buf, &key_expanded);
                }
                assert_eq!(avx_buf, ref_buf, "AVX2 mismatch at len {len}");
            }

            // Test fallback path directly.
            let mut fb_buf = orig.clone();
            apply_chunk_fallback(&mut fb_buf, &key_expanded);
            assert_eq!(fb_buf, ref_buf, "fallback mismatch at len {len}");
        }
    }
}
