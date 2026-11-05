// Drop-in замена librqbit-sha1-wrapper на чистом Rust (RustCrypto).
// Исходный API: трейты ISha1/ISha256 + типы Sha1/Sha256.
// Используется librqbit 9.0.1 (infohash, хеширование кусков, BEP52).

pub trait ISha1 {
    fn new() -> Self;
    fn update(&mut self, buf: &[u8]);
    fn finish(self) -> [u8; 20];
}

/// SHA-256 трейт для BEP 52 (BitTorrent v2).
pub trait ISha256 {
    fn new() -> Self;
    fn update(&mut self, buf: &[u8]);
    fn finish(self) -> [u8; 32];

    fn finish_id32(self) -> [u8; 32]
    where
        Self: Sized,
    {
        self.finish()
    }
}

pub struct Sha1(sha1::Sha1);

impl ISha1 for Sha1 {
    fn new() -> Self {
        Self(<sha1::Sha1 as sha1::Digest>::new())
    }
    fn update(&mut self, buf: &[u8]) {
        sha1::Digest::update(&mut self.0, buf);
    }
    fn finish(self) -> [u8; 20] {
        sha1::Digest::finalize(self.0).into()
    }
}

pub struct Sha256(sha2::Sha256);

impl ISha256 for Sha256 {
    fn new() -> Self {
        Self(<sha2::Sha256 as sha2::Digest>::new())
    }
    fn update(&mut self, buf: &[u8]) {
        sha2::Digest::update(&mut self.0, buf);
    }
    fn finish(self) -> [u8; 32] {
        sha2::Digest::finalize(self.0).into()
    }
}

#[cfg(test)]
mod tests {
    use super::{ISha1, ISha256, Sha1, Sha256};

    #[test]
    fn sha1_known_vector() {
        let mut h = Sha1::new();
        h.update(b"abc");
        assert_eq!(
            h.finish(),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78,
                0x50, 0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ]
        );
    }

    #[test]
    fn sha256_known_vector() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            h.finish()[..8],
            [0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea][..]
        );
    }
}
