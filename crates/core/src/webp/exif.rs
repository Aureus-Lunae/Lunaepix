//! The EXIF Orientation tag of a WebP `EXIF` chunk or a JPEG `APP1` segment,
//! read in safe Rust.
//!
//! The payload is a TIFF stream, sometimes after the `Exif\0\0` prefix a JPEG
//! uses. Only IFD0 is read, and in it only the Orientation entry. Every
//! offset is checked against the payload and the number of entries read is
//! capped by what fits, so a hostile stream costs at most the 65,535 entries
//! a count can declare, and anything malformed reads as "no rotation".
//! [`is_rotated`] asks the optimiser's question (does it turn the picture),
//! [`orientation`] a thumbnail's (which of the eight turns).

/// The prefix some writers put before the TIFF stream, as in a JPEG APP1.
pub(crate) const PREFIX: &[u8] = b"Exif\0\0";

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

/// The value of every well-formed Orientation entry of the EXIF `payload`'s
/// IFD0, in file order: type SHORT, count 1, the 16-bit value as stored.
///
/// Every other shape gives nothing: no TIFF header, an IFD0 offset that
/// points into the header or past the end, an entry of another tag, type or
/// count. Only the entries that fit in the payload are read, however many
/// the count declares.
fn orientation_values(payload: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let tiff = payload.strip_prefix(PREFIX).unwrap_or(payload);
    ifd0(tiff).into_iter().flat_map(|(order, count, entries)| {
        // Whole entries only: a partial one at the end is not read.
        entries.iter().take(count).filter_map(move |entry| {
            let &[t0, t1, y0, y1, k0, k1, k2, k3, v0, v1, _, _] = entry;
            let wanted = order.u16([t0, t1]) == ORIENTATION
                && order.u16([y0, y1]) == SHORT
                && order.u32([k0, k1, k2, k3]) == 1;
            // A SHORT sits in the first two bytes of the value field.
            wanted.then(|| order.u16([v0, v1]))
        })
    })
}

/// IFD0 of the TIFF stream `tiff`: its byte order, the entry count it
/// declares and the whole entries that fit behind the count. `None` when
/// the header is cut short, the byte-order mark or the magic number is
/// wrong, or IFD0 starts inside the header or without room for its count.
fn ifd0(tiff: &[u8]) -> Option<(Order, usize, &[[u8; ENTRY]])> {
    let &[m0, m1, n0, n1, o0, o1, o2, o3] = tiff.first_chunk::<TIFF_HEADER>()?;
    let order = match [m0, m1] {
        [b'I', b'I'] => Order::Little,
        [b'M', b'M'] => Order::Big,
        _ => return None,
    };
    if order.u16([n0, n1]) != 42 {
        return None;
    }
    // Lossless: `usize` has at least 32 bits on every target this builds for.
    let ifd = order.u32([o0, o1, o2, o3]) as usize;
    if ifd < TIFF_HEADER {
        return None;
    }
    // The 2-byte entry count, then the entries; both inside the stream.
    let (&[c0, c1], entries) = tiff.get(ifd..)?.split_first_chunk::<2>()?;
    let count = usize::from(order.u16([c0, c1]));
    Some((order, count, entries.as_chunks::<ENTRY>().0))
}

/// Whether the EXIF `payload` turns the picture: its IFD0 holds an
/// Orientation entry of type SHORT, count 1, whose value is not 1
/// ([`orientation_values`]).
///
/// When IFD0 holds the tag more than once, any well-formed entry that is
/// not 1 is enough, so no reader's choice of entry can lose a rotation. The
/// value is not checked against 2 to 8: a tag that is there and not 1
/// counts, as the ruling says.
pub(super) fn is_rotated(payload: &[u8]) -> bool {
    orientation_values(payload).any(|value| value != 1)
}

/// The EXIF Orientation of the `payload` as 1 to 8, as a thumbnail turns
/// the picture: the first well-formed Orientation entry of IFD0
/// ([`orientation_values`]), which is what a reader that takes the first
/// entry shows.
///
/// `None` when there is none, or when the first one holds a value outside
/// 1 to 8; a later entry is not looked at then, so a damaged value is never
/// replaced by a guess.
pub(crate) fn orientation(payload: &[u8]) -> Option<u8> {
    let value = orientation_values(payload).next()?;
    u8::try_from(value)
        .ok()
        .filter(|value| (1..=8).contains(value))
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

    // --- orientation -------------------------------------------------------------

    /// `orientation`, which must come back quickly whatever the input declares.
    fn oriented_as(payload: &[u8]) -> Option<u8> {
        let started = Instant::now();
        let result = orientation(payload);
        assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
        result
    }

    #[test]
    fn reads_each_orientation_from_1_to_8_in_both_byte_orders() {
        for big_endian in [false, true] {
            for value in 1..=8u8 {
                assert_eq!(
                    oriented_as(&oriented(u16::from(value), big_endian)),
                    Some(value),
                    "orientation {value}, big endian: {big_endian}"
                );
            }
        }
    }

    #[test]
    fn finds_no_orientation_for_0_or_above_8() {
        for big_endian in [false, true] {
            for value in [0, 9, 255, 256, 1536, u16::MAX] {
                assert_eq!(
                    oriented_as(&oriented(value, big_endian)),
                    None,
                    "value {value}, big endian: {big_endian}"
                );
            }
        }
    }

    #[test]
    fn reads_the_orientation_after_other_tags_in_a_camera_payload() {
        for (big_endian, prefix) in [(false, true), (true, false), (false, false), (true, true)] {
            assert_eq!(
                oriented_as(&fx::exif_with_orientation(6, big_endian, prefix)),
                Some(6)
            );
            assert_eq!(
                oriented_as(&fx::exif_with_orientation(1, big_endian, prefix)),
                Some(1)
            );
        }
    }

    #[test]
    fn reads_the_orientation_with_and_without_the_exif_prefix() {
        let stream = oriented(8, false);
        let mut prefixed = fx::EXIF_PREFIX.to_vec();
        prefixed.extend_from_slice(&stream);
        assert_eq!(oriented_as(&stream), Some(8));
        assert_eq!(oriented_as(&prefixed), Some(8));
        // A prefix that is almost `Exif\0\0` is not skipped, nor is it twice.
        let mut near = b"Exif\0X".to_vec();
        near.extend_from_slice(&stream);
        assert_eq!(oriented_as(&near), None);
        let mut twice = fx::EXIF_PREFIX.to_vec();
        twice.extend_from_slice(&prefixed);
        assert_eq!(oriented_as(&twice), None);
    }

    #[test]
    fn finds_no_orientation_without_the_tag() {
        for big_endian in [false, true] {
            let other_tags = fx::tiff(
                big_endian,
                &[(0x0100, fx::SHORT, 1, 6), (0x0101, fx::SHORT, 1, 6)],
            );
            assert_eq!(oriented_as(&other_tags), None);
            assert_eq!(oriented_as(&fx::tiff(big_endian, &[])), None);
        }
    }

    #[test]
    fn finds_no_orientation_in_what_is_not_a_tiff_stream() {
        let good = oriented(6, false);
        let mut wrong_order_mark = good.clone();
        wrong_order_mark[..2].copy_from_slice(b"IM");
        let mut wrong_magic = good.clone();
        wrong_magic[2] = 43;
        let mut mixed = oriented(6, true);
        mixed[2..4].copy_from_slice(&42u16.to_le_bytes());
        for case in [
            &b""[..],
            b"Exif\0\0",
            &wrong_order_mark,
            &wrong_magic,
            &mixed,
        ] {
            assert_eq!(oriented_as(case), None, "{case:?}");
        }
        assert_eq!(oriented_as(&good), Some(6));
    }

    #[test]
    fn finds_no_orientation_of_another_type_than_short_or_a_count_other_than_1() {
        for big_endian in [false, true] {
            for kind in [1, 4, 9, 7] {
                let stream = fx::tiff(big_endian, &[(fx::ORIENTATION, kind, 1, 6)]);
                assert_eq!(oriented_as(&stream), None, "type {kind}");
            }
            for count in [0, 2, u32::MAX] {
                let stream = fx::tiff(big_endian, &[(fx::ORIENTATION, fx::SHORT, count, 6)]);
                assert_eq!(oriented_as(&stream), None, "count {count}");
            }
        }
    }

    #[test]
    fn finds_no_orientation_in_a_stream_cut_short() {
        for big_endian in [false, true] {
            let whole = oriented(6, big_endian);
            // The header (8), the count (2) and the one entry (12).
            let entry_end = 8 + 2 + 12;
            for len in 0..whole.len() {
                let expected = (len >= entry_end).then_some(6);
                assert_eq!(
                    oriented_as(&whole[..len]),
                    expected,
                    "cut to {len} bytes, big endian: {big_endian}"
                );
            }
        }
    }

    #[test]
    fn finds_no_orientation_when_ifd0_points_past_the_end_or_into_the_header() {
        for big_endian in [false, true] {
            let stream = oriented(6, big_endian);
            let len = stream.len() as u32;
            for offset in [len, len - 1, len + 1, u32::MAX, 0, 4, 7] {
                let moved = with_ifd_offset(stream.clone(), big_endian, offset);
                assert_eq!(oriented_as(&moved), None, "offset {offset}");
            }
            assert_eq!(oriented_as(&stream), Some(6));
        }
    }

    #[test]
    fn reads_an_orientation_only_from_the_entries_that_fit_whatever_the_count_says() {
        for big_endian in [false, true] {
            let one_there = with_entry_count(oriented(6, big_endian), big_endian, u16::MAX);
            assert_eq!(oriented_as(&one_there), Some(6));
            let none_there = with_entry_count(fx::tiff(big_endian, &[]), big_endian, u16::MAX);
            assert_eq!(oriented_as(&none_there), None);
            let not_counted = with_entry_count(oriented(6, big_endian), big_endian, 0);
            assert_eq!(oriented_as(&not_counted), None);
            let second = fx::tiff(
                big_endian,
                &[
                    (0x0100, fx::SHORT, 1, 640),
                    (fx::ORIENTATION, fx::SHORT, 1, 6),
                ],
            );
            assert_eq!(oriented_as(&second), Some(6));
            assert_eq!(oriented_as(&with_entry_count(second, big_endian, 1)), None);
        }
    }

    #[test]
    fn takes_the_first_well_formed_orientation_entry() {
        let entry = |value| (fx::ORIENTATION, fx::SHORT, 1, value);
        let wrong_type = (fx::ORIENTATION, 4, 1, 6);
        for big_endian in [false, true] {
            // A malformed entry ahead of it is skipped; the later ones lose.
            let stream = fx::tiff(big_endian, &[wrong_type, entry(8), entry(3)]);
            assert_eq!(oriented_as(&stream), Some(8));
            // The first well-formed entry decides even when it is out of
            // range: the 6 behind it is not used.
            let stream = fx::tiff(big_endian, &[entry(9), entry(6)]);
            assert_eq!(oriented_as(&stream), None);
        }
    }
}
