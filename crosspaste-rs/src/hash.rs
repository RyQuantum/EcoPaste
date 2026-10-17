//! CrossPaste content hashes: MurmurHash3 x64 128-bit with a fixed seed,
//! rendered as 32 lowercase hex chars (each 64-bit half little-endian).
//! Mirrors `CodecsUtils.hash` / `FileUtils.getFileHash` in CrossPaste.

pub const CROSSPASTE_SEED: u32 = 13_043_025;

const C1: u64 = 0x87c3_7b91_1142_53d5;
const C2: u64 = 0x4cf5_ad43_2745_937f;

fn fmix(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

/// Standard MurmurHash3_x64_128.
pub fn murmur3_x64_128(data: &[u8], seed: u32) -> (u64, u64) {
    let mut h1 = seed as u64;
    let mut h2 = seed as u64;
    let blocks = data.len() / 16;

    for i in 0..blocks {
        let b = &data[i * 16..i * 16 + 16];
        let k1 = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let k2 = u64::from_le_bytes(b[8..16].try_into().unwrap());

        h1 ^= k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 = h1
            .rotate_left(27)
            .wrapping_add(h2)
            .wrapping_mul(5)
            .wrapping_add(0x52dc_e729);

        h2 ^= k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 = h2
            .rotate_left(31)
            .wrapping_add(h1)
            .wrapping_mul(5)
            .wrapping_add(0x3849_5ab5);
    }

    let tail = &data[blocks * 16..];
    let mut k1: u64 = 0;
    let mut k2: u64 = 0;
    for (i, byte) in tail.iter().enumerate() {
        if i < 8 {
            k1 |= (*byte as u64) << (i * 8);
        } else {
            k2 |= (*byte as u64) << ((i - 8) * 8);
        }
    }
    if tail.len() > 8 {
        h2 ^= k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
    }
    if !tail.is_empty() {
        h1 ^= k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
    }

    let len = data.len() as u64;
    h1 ^= len;
    h2 ^= len;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix(h1);
    h2 = fmix(h2);
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    (h1, h2)
}

/// `CodecsUtils.hash`: empty input hashes to the empty string.
pub fn hash(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }
    let (h1, h2) = murmur3_x64_128(data, CROSSPASTE_SEED);
    let mut out = String::with_capacity(32);
    for byte in h1.to_le_bytes().iter().chain(h2.to_le_bytes().iter()) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `CodecsUtils.hashByArray`: hash of the UTF-8 concatenation.
pub fn hash_strings(parts: &[&str]) -> String {
    hash(parts.concat().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_empty_string() {
        assert_eq!(hash(b""), "");
    }

    #[test]
    fn single_part_array_equals_plain_hash() {
        assert_eq!(hash_strings(&["abc"]), hash(b"abc"));
        assert_eq!(hash_strings(&["ab", "c"]), hash(b"abc"));
    }

    /// Vectors produced by CrossPaste 2.2.1's own `DesktopCoreCodecsUtils` (via jshell).
    #[test]
    fn matches_crosspaste_vectors() {
        assert_eq!(hash(b"hello"), "e58075f7d20aa573c58a3c352a967db7");
        assert_eq!(
            hash(b"The quick brown fox jumps over the lazy dog!"),
            "c98b5f572f4f2f0d5f2d014710d0ceea"
        );
        assert_eq!(
            hash("你好，世界 CrossPaste".as_bytes()),
            "f024f601c36151770659a55962762114"
        );
        let block: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(17)).collect();
        assert_eq!(hash(&block), "f6264924752846dfc6fa99a58165cc42");
        assert_eq!(
            hash_strings(&["abc", "def"]),
            "ded45fdc26f49ee2c34e5251500a4c4e"
        );
    }
}
