//! The EXIF Orientation tag of a WebP `EXIF` chunk, read in safe Rust.
//!
//! The payload is a TIFF stream, sometimes after the `Exif\0\0` prefix a JPEG
//! uses. Only IFD0 is read, and in it only the Orientation entry. Every
//! offset is checked against the payload and the number of entries read is
//! capped by what fits, so a hostile stream costs at most the 65,535 entries
//! a count can declare, and anything malformed reads as "no rotation".

/// The prefix some writers put before the TIFF stream, as in a JPEG APP1.
const PREFIX: &[u8] = b"Exif\0\0";

/// The TIFF header: byte order mark, the magic number 42, the IFD0 offset.
const TIFF_HEADER: usize = 8;

/// One IFD entry: tag, type, count and a 4-byte value field.
const ENTRY: usize = 12;

/// The Orientation tag, and SHORT, the only type its value may have.
const ORIENTATION: u16 = 0x0112;
const SHORT: u16 = 3;

/// The byte order the stream declares, `II` or `MM`.
#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Order::Little => u16::from_le_bytes(bytes),
            Order::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Order::Little => u32::from_le_bytes(bytes),
            Order::Big => u32::from_be_bytes(bytes),
        }
    }
}

/// Whether the EXIF `payload` turns the picture: its IFD0 holds an
/// Orientation entry of type SHORT, count 1, whose value is not 1.
///
/// Every other shape is "no": no TIFF header, an IFD0 offset that points
/// into the header or past the end, an Orientation of another type or count,
/// or none at all. Only the entries that fit in the payload are read,
/// however many the count declares. When IFD0 holds the tag more than once,
/// any well-formed entry that is not 1 is enough, so no reader's choice of
/// entry can lose a rotation. The value is not checked against 2 to 8:
/// a tag that is there and not 1 counts, as the ruling says.
pub(super) fn is_rotated(payload: &[u8]) -> bool {
    let tiff = payload.strip_prefix(PREFIX).unwrap_or(payload);
    let Some(&[m0, m1, n0, n1, o0, o1, o2, o3]) = tiff.first_chunk::<TIFF_HEADER>() else {
        return false;
    };
    let order = match [m0, m1] {
        [b'I', b'I'] => Order::Little,
        [b'M', b'M'] => Order::Big,
        _ => return false,
    };
    if order.u16([n0, n1]) != 42 {
        return false;
    }
    // Lossless: `usize` has at least 32 bits on every target this builds for.
    let ifd = order.u32([o0, o1, o2, o3]) as usize;
    if ifd < TIFF_HEADER {
        return false;
    }
    // The 2-byte entry count, then the entries; both inside the stream.
    let Some((&[c0, c1], entries)) = tiff.get(ifd..).and_then(<[u8]>::split_first_chunk::<2>)
    else {
        return false;
    };
    let count = usize::from(order.u16([c0, c1]));
    // Whole entries only: a partial one at the end is not read.
    let (whole, _) = entries.as_chunks::<ENTRY>();
    whole.iter().take(count).any(|entry| {
        let &[t0, t1, y0, y1, k0, k1, k2, k3, v0, v1, _, _] = entry;
        order.u16([t0, t1]) == ORIENTATION
            && order.u16([y0, y1]) == SHORT
            && order.u32([k0, k1, k2, k3]) == 1
            // A SHORT sits in the first two bytes of the value field.
            && order.u16([v0, v1]) != 1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webp::fixtures as fx;
    use std::time::{Duration, Instant};

    /// Generous: the read takes microseconds whatever the stream declares.
    const QUICK: Duration = Duration::from_secs(5);

    /// A stream whose IFD0 holds only the Orientation, as SHORT with count 1.
    fn oriented(orientation: u16, big_endian: bool) -> Vec<u8> {
        fx::tiff(big_endian, &[(fx::ORIENTATION, fx::SHORT, 1, orientation)])
    }

    /// `stream` with its IFD0 offset (bytes 4..8) replaced.
    fn with_ifd_offset(mut stream: Vec<u8>, big_endian: bool, offset: u32) -> Vec<u8> {
        let bytes = if big_endian {
            offset.to_be_bytes()
        } else {
            offset.to_le_bytes()
        };
        stream[4..8].copy_from_slice(&bytes);
        stream
    }

    /// `stream` with the entry count of its IFD0 (at offset 8) replaced.
    fn with_entry_count(mut stream: Vec<u8>, big_endian: bool, count: u16) -> Vec<u8> {
        let bytes = if big_endian {
            count.to_be_bytes()
        } else {
            count.to_le_bytes()
        };
        stream[8..10].copy_from_slice(&bytes);
        stream
    }

    /// `is_rotated`, which must come back quickly whatever the input declares.
    fn rotated(payload: &[u8]) -> bool {
        let started = Instant::now();
        let result = is_rotated(payload);
        assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
        result
    }

    #[test]
    fn finds_a_rotation_in_both_byte_orders() {
        for big_endian in [false, true] {
            for orientation in 2..=8 {
                assert!(
                    rotated(&oriented(orientation, big_endian)),
                    "orientation {orientation}, big endian: {big_endian}"
                );
            }
        }
    }

    #[test]
    fn finds_no_rotation_in_orientation_1() {
        for big_endian in [false, true] {
            // A big-endian 1 read as little-endian would be 256: "rotated".
            assert!(
                !rotated(&oriented(1, big_endian)),
                "big endian: {big_endian}"
            );
        }
    }

    #[test]
    fn finds_no_rotation_without_an_orientation_tag() {
        for big_endian in [false, true] {
            let other_tags = fx::tiff(
                big_endian,
                &[(0x0100, fx::SHORT, 1, 6), (0x0101, fx::SHORT, 1, 6)],
            );
            assert!(!rotated(&other_tags));
            assert!(!rotated(&fx::tiff(big_endian, &[])));
        }
    }

    #[test]
    fn finds_the_rotation_after_other_tags_in_a_camera_payload() {
        for (big_endian, prefix) in [(false, true), (true, false), (false, false), (true, true)] {
            assert!(rotated(&fx::exif_with_orientation(6, big_endian, prefix)));
            assert!(!rotated(&fx::exif_with_orientation(1, big_endian, prefix)));
        }
    }

    #[test]
    fn reads_the_stream_with_and_without_the_exif_prefix() {
        let stream = oriented(6, false);
        let mut prefixed = fx::EXIF_PREFIX.to_vec();
        prefixed.extend_from_slice(&stream);
        assert!(rotated(&stream));
        assert!(rotated(&prefixed));

        // A prefix that is almost `Exif\0\0` is not skipped.
        let mut near = b"Exif\0X".to_vec();
        near.extend_from_slice(&stream);
        assert!(!rotated(&near));
        // Nor is the prefix twice.
        let mut twice = fx::EXIF_PREFIX.to_vec();
        twice.extend_from_slice(&prefixed);
        assert!(!rotated(&twice));
    }

    #[test]
    fn finds_no_rotation_in_what_is_not_a_tiff_stream() {
        let good = oriented(6, false);
        let mut wrong_order_mark = good.clone();
        wrong_order_mark[..2].copy_from_slice(b"IM");
        let mut wrong_magic = good.clone();
        wrong_magic[2] = 43;
        // `MM` with the magic number written little-endian.
        let mut mixed = oriented(6, true);
        mixed[2..4].copy_from_slice(&42u16.to_le_bytes());
        for case in [
            &b""[..],
            b"Exif\0\0",
            &wrong_order_mark,
            &wrong_magic,
            &mixed,
        ] {
            assert!(!rotated(case), "{case:?}");
        }
    }

    #[test]
    fn ignores_an_orientation_of_another_type_than_short() {
        for big_endian in [false, true] {
            // BYTE, LONG, SLONG and UNDEFINED, each with the value 6 where a
            // SHORT keeps it.
            for kind in [1, 4, 9, 7] {
                let stream = fx::tiff(big_endian, &[(fx::ORIENTATION, kind, 1, 6)]);
                assert!(!rotated(&stream), "type {kind}, big endian: {big_endian}");
            }
            assert!(rotated(&fx::tiff(
                big_endian,
                &[(fx::ORIENTATION, fx::SHORT, 1, 6)]
            )));
        }
    }

    #[test]
    fn ignores_an_orientation_whose_count_is_not_1() {
        for big_endian in [false, true] {
            for count in [0, 2, u32::MAX] {
                let stream = fx::tiff(big_endian, &[(fx::ORIENTATION, fx::SHORT, count, 6)]);
                assert!(!rotated(&stream), "count {count}");
            }
        }
    }

    #[test]
    fn finds_no_rotation_in_a_stream_cut_short() {
        for big_endian in [false, true] {
            let whole = oriented(6, big_endian);
            // The header (8), the count (2) and the one entry (12).
            let entry_end = 8 + 2 + 12;
            for len in 0..whole.len() {
                assert_eq!(
                    rotated(&whole[..len]),
                    len >= entry_end,
                    "cut to {len} bytes, big endian: {big_endian}"
                );
            }
            assert!(rotated(&whole));
        }
    }

    #[test]
    fn finds_no_rotation_when_ifd0_points_past_the_end() {
        for big_endian in [false, true] {
            let stream = oriented(6, big_endian);
            let len = stream.len() as u32;
            for offset in [len, len - 1, len + 1, u32::MAX] {
                let moved = with_ifd_offset(stream.clone(), big_endian, offset);
                assert!(!rotated(&moved), "offset {offset}");
            }
        }
    }

    #[test]
    fn finds_no_rotation_when_ifd0_points_into_the_header() {
        // IFD0 at 4 (inside the 8-byte header) reads the count from the
        // offset field itself (4) and entries from byte 6 on, so an
        // Orientation-6 entry at byte 18 would be found by a reader that
        // allows the offset. It must not be.
        let mut stream = b"II\x2A\x00\x04\x00\x00\x00".to_vec();
        stream.extend_from_slice(&[0; 10]);
        stream.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        stream.extend_from_slice(&[0; 24]);
        assert!(!rotated(&stream));
        // The same entry where IFD0 really is: found.
        let mut proper = b"II\x2A\x00\x08\x00\x00\x00\x01\x00".to_vec();
        proper.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        assert!(rotated(&proper));

        for big_endian in [false, true] {
            for offset in 0..8 {
                let moved = with_ifd_offset(oriented(6, big_endian), big_endian, offset);
                assert!(!rotated(&moved), "offset {offset}");
            }
        }
    }

    #[test]
    fn reads_only_the_entries_that_fit_whatever_the_count_says() {
        for big_endian in [false, true] {
            // 65,535 entries declared, one there: it is read, quickly.
            let one_there = with_entry_count(oriented(6, big_endian), big_endian, u16::MAX);
            assert!(rotated(&one_there));
            // 65,535 declared, none there.
            let none_there = with_entry_count(fx::tiff(big_endian, &[]), big_endian, u16::MAX);
            assert!(!rotated(&none_there));
            // The count is honoured: an entry behind a count of 0 is not read.
            let not_counted = with_entry_count(oriented(6, big_endian), big_endian, 0);
            assert!(!rotated(&not_counted));
            // An entry beyond the count is not read either.
            let second = fx::tiff(
                big_endian,
                &[
                    (0x0100, fx::SHORT, 1, 640),
                    (fx::ORIENTATION, fx::SHORT, 1, 6),
                ],
            );
            assert!(rotated(&second));
            assert!(!rotated(&with_entry_count(second, big_endian, 1)));
        }
    }

    #[test]
    fn keeps_a_rotation_when_any_orientation_entry_rotates() {
        // The rule for duplicates: any well-formed Orientation entry that is
        // not 1 counts, wherever it is, so no reader's choice of entry can
        // lose a rotation.
        let entry = |value| (fx::ORIENTATION, fx::SHORT, 1, value);
        for big_endian in [false, true] {
            assert!(rotated(&fx::tiff(big_endian, &[entry(1), entry(6)])));
            assert!(rotated(&fx::tiff(big_endian, &[entry(8), entry(1)])));
            assert!(!rotated(&fx::tiff(big_endian, &[entry(1), entry(1)])));
            // A malformed duplicate does not count.
            let wrong_type = (fx::ORIENTATION, 4, 1, 6);
            assert!(!rotated(&fx::tiff(big_endian, &[wrong_type, entry(1)])));
        }
    }
}
