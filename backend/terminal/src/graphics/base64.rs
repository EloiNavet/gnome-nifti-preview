//! Base64 (RFC 4648, standard alphabet, padded), for Kitty and iTerm2.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63]);
        out.push(ALPHABET[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] } else { b'=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] } else { b'=' });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc4648_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::encode(i.as_bytes()), o.as_bytes());
        }
        assert_eq!(super::encode(&[0xff, 0xfe, 0x00]), b"//4A");
    }
}
