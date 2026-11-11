//! JPEG fixtures put together by hand, for what the `image` crate cannot
//! write.
#![allow(dead_code)]

/// The Adobe APP14 colour transform of a four-component JPEG: libjpeg reads
/// 0 as CMYK and 2 as YCCK.
pub const ADOBE_CMYK: u8 = 0;
pub const ADOBE_YCCK: u8 = 2;

/// A 16x16 four-component baseline JPEG (the `image` crate cannot write
/// CMYK): an Adobe APP14 marker with `transform` ([`ADOBE_CMYK`] or
/// [`ADOBE_YCCK`]), four components at 1x1, and a scan in which every
/// block is flat. Two one-code Huffman tables make each block two bits: DC
/// difference 0, then end of block.
pub fn four_component_jpeg(transform: u8) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8];
    // APP14 "Adobe": version 100, flags 0 and 0, then the transform.
    bytes.extend_from_slice(&[0xFF, 0xEE, 0x00, 0x0E]);
    bytes.extend_from_slice(b"Adobe");
    bytes.extend_from_slice(&[0x00, 0x64, 0x00, 0x00, 0x00, 0x00, transform]);
    // DQT: table 0, 8-bit, all ones.
    bytes.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
    bytes.extend_from_slice(&[1; 64]);
    // SOF0: precision 8, 16x16, four components (ids 1-4, 1x1, table 0).
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x14, 0x08, 0x00, 0x10, 0x00, 0x10, 0x04]);
    for id in 1..=4 {
        bytes.extend_from_slice(&[id, 0x11, 0x00]);
    }
    // DHT: DC table 0 and AC table 0, each one code of one bit for symbol 0.
    for class in [0x00, 0x10] {
        bytes.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x14, class, 1]);
        bytes.extend_from_slice(&[0; 15]);
        bytes.push(0);
    }
    // SOS: four components, tables 0/0, full spectrum.
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x0E, 0x04]);
    for id in 1..=4 {
        bytes.extend_from_slice(&[id, 0x00]);
    }
    bytes.extend_from_slice(&[0x00, 0x3F, 0x00]);
    // Four MCUs of four blocks, two zero bits per block.
    bytes.extend_from_slice(&[0x00; 4]);
    bytes.extend_from_slice(&[0xFF, 0xD9]);
    bytes
}

/// A greyscale JPEG header of `w`x`h` with an empty scan: libjpeg reads the
/// header, and there is no image data to decode.
pub fn grey_header(w: u16, h: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
    bytes.extend_from_slice(&[1; 64]);
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xD9]);
    bytes
}
