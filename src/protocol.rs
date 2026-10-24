//! DS916 USB protocol: the screen is a CDC ACM virtual COM port. Frames are
//! JPEGs wrapped in a 60-byte header. Only bytes 0, 8-11 and 56-59 change; the
//! rest are fixed magic values.

pub const HEADER_LEN: usize = 60;

const HEADER_TPL: [u8; HEADER_LEN] = [
    0x00, 0x3c, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x4f, 0x54, 0x06, //
    0x00, 0x00, 0x00, 0x00, //
    0x1b, //
    0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, //
    0x1b, 0x00, //
    0x00, 0x00, 0x00, 0x00, //
    0x89, 0xb3, 0xff, 0xff, 0x00, //
    0x00, 0x00, 0x00, 0x09, 0x00, 0x00, //
    0x01, 0x00, 0x03, 0x00, 0x02, 0x03, //
    0x00, 0x00, 0x00, 0x00,
];

/// Header + JPEG into `out` (reused between frames). `first` = first frame
/// after (re)connecting: byte 0 is 0x03 then, 0x00 afterwards.
pub fn make_frame(jpeg: &[u8], first: bool, out: &mut Vec<u8>) {
    let mut h = HEADER_TPL;
    h[0] = if first { 0x03 } else { 0x00 };
    h[8..12].copy_from_slice(&((jpeg.len() + HEADER_LEN) as u32).to_le_bytes());
    h[56..60].copy_from_slice(&(jpeg.len() as u32).to_le_bytes());
    out.clear();
    out.extend_from_slice(&h);
    out.extend_from_slice(jpeg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout() {
        let jpeg = [0xFFu8; 100];
        let mut f = Vec::new();
        make_frame(&jpeg, true, &mut f);
        assert_eq!(f.len(), 160);
        assert_eq!(f[0], 0x03);
        assert_eq!(f[1], 0x3c);
        assert_eq!(f[5], 0x06);
        assert_eq!(f[8..12], 160u32.to_le_bytes());
        assert_eq!(f[56..60], 100u32.to_le_bytes());
        assert_eq!(&f[60..], &jpeg[..]);
        make_frame(&[1u8; 50], false, &mut f);
        assert_eq!(f[0], 0x00);
        assert_eq!(f.len(), 110);
    }
}
