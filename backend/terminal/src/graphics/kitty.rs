//! Kitty's graphics protocol: a PNG transmitted directly in the escape
//! codes (`t=d`, never a file path, which a terminal across SSH could not
//! read), scaled by the terminal to `c × r` cells, in chunks of 4096
//! base64 bytes.

use super::base64;

pub const CHUNK: usize = 4096;

/// The APC sequences transmitting and showing a PNG over `cols × rows`
/// cells; `id` (when not 0) replaces the image of that id.
pub fn transmit(png: &[u8], id: u32, cols: usize, rows: usize) -> Vec<Vec<u8>> {
    let data = base64::encode(png);
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&data[..]] } else { data.chunks(CHUNK).collect() };
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(k, chunk)| {
            let more = u8::from(k + 1 < n);
            let mut s = if k == 0 {
                let id = if id != 0 { format!(",i={id}") } else { String::new() };
                format!("\x1b_Ga=T,f=100,t=d,q=2,C=1,c={cols},r={rows}{id},m={more};").into_bytes()
            } else {
                format!("\x1b_Gm={more};").into_bytes()
            };
            s.extend_from_slice(chunk);
            s.extend_from_slice(b"\x1b\\");
            s
        })
        .collect()
}

/// Remove the image `id` and free its data.
pub fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// Remove every image this program placed.
pub fn delete_all() -> Vec<u8> {
    b"\x1b_Ga=d,d=A,q=2\x1b\\".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_and_keys() {
        let one = transmit(b"PNG", 7, 20, 10);
        assert_eq!(one, vec![b"\x1b_Ga=T,f=100,t=d,q=2,C=1,c=20,r=10,i=7,m=0;UE5H\x1b\\".to_vec()]);
        // 3 × 4096 base64 bytes + 4: four chunks, only the last with m=0.
        let png = vec![0u8; 3 * 3072 + 3];
        let many = transmit(&png, 0, 1, 1);
        assert_eq!(many.len(), 4);
        assert!(many[0].starts_with(b"\x1b_Ga=T,f=100,t=d,q=2,C=1,c=1,r=1,m=1;"));
        assert!(many[1].starts_with(b"\x1b_Gm=1;") && many[2].starts_with(b"\x1b_Gm=1;") && many[3].starts_with(b"\x1b_Gm=0;"));
        let payload: usize = many.iter().map(|c| c.len() - c.iter().position(|&b| b == b';').unwrap() - 3).sum();
        assert_eq!(payload, 4 * 3072 + 4);
        assert_eq!(delete(7), b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    }
}
