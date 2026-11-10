//! The RIFF container of a WebP file, walked in safe Rust.
//!
//! The walk reads chunk headers and the few header bytes of the image chunks
//! (`VP8X`, `VP8L`, `VP8 `) and nothing else. It never decodes an image, so a
//! file that declares a huge image is refused from its headers alone.

use super::exif;
use crate::plan::check_pixels;

/// Most chunks one file may hold. A real file has a handful, an animation one
/// `ANMF` per frame; the limit only stops a file of nothing but empty chunks
/// (13 million of them fit in [`crate::MAX_FILE_BYTES`]) from growing the
/// chunk list to hundreds of megabytes.
const MAX_CHUNKS: usize = 1_000_000;

/// The error for content that is not WebP data at all.
const NOT_WEBP: &str = "not a WebP file (the content is not WebP data)";

/// The size of the `RIFF` header: `"RIFF"`, the size field and `"WEBP"`.
const RIFF_HEADER: usize = 12;

/// The `VP8X` payload: flags, 3 reserved bytes, then the two canvas sizes.
const VP8X_LEN: usize = 10;

/// What a file holds, as far as the optimiser needs to tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A still `VP8 ` image, with or without an `ALPH` chunk.
    Lossy,
    /// A still `VP8L` image.
    Lossless,
    /// An `ANIM` chunk and at least one `ANMF` frame.
    Animated,
}

/// One chunk: its FourCC and its payload, without the padding byte an odd
/// size is followed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chunk<'a> {
    pub fourcc: [u8; 4],
    pub data: &'a [u8],
}

/// A parsed WebP file: its chunks in file order, and what kind of file it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebpFile<'a> {
    pub chunks: Vec<Chunk<'a>>,
    /// Read by the lossy path, which a run calls from a later task on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub kind: Kind,
}

/// The chunks a stripped file always keeps: the image, its alpha, the
/// animation and the colour profile, and the `VP8X` that describes them.
/// The first `EXIF` chunk is kept too when it turns the picture (see
/// [`exif::is_rotated`]). Every other chunk is dropped: `XMP `, the other
/// `EXIF` chunks and whatever else a writer put in.
const KEPT: [&[u8; 4]; 7] = [
    b"VP8X", b"ICCP", b"ANIM", b"ANMF", b"ALPH", b"VP8 ", b"VP8L",
];

/// `VP8X` flag bits: the chunks a file says it holds.
const FLAG_ICC: u8 = 0x20;
const FLAG_ALPHA: u8 = 0x10;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;
const FLAG_ANIMATION: u8 = 0x02;

impl WebpFile<'_> {
    /// The file rewritten with only the kept chunks (see [`KEPT`]), in their
    /// original order, each followed by the pad byte its odd size needs, even
    /// when the source left the last one out. The image chunks are copied byte
    /// for byte; nothing is decoded.
    ///
    /// The `VP8X` flags for EXIF and XMP are set from the chunks that are
    /// left, whatever the source's flags said. The ICC flag is the source's
    /// own, set or clear, when an `ICCP` chunk is left, so colour management
    /// is exactly as before; with no `ICCP` chunk it is cleared. Every other
    /// bit, the reserved bytes and the canvas are as they were. A file with
    /// no `VP8X` stays without one. The RIFF size is written to match, so
    /// bytes the source had after its own RIFF size are not in the result.
    pub(crate) fn stripped(&self) -> Vec<u8> {
        let rotated_exif = self.rotated_exif();
        let kept = self
            .chunks
            .iter()
            .enumerate()
            .filter(|&(at, chunk)| KEPT.contains(&&chunk.fourcc) || Some(at) == rotated_exif)
            .map(|(_, chunk)| chunk);

        let mut body = b"WEBP".to_vec();
        for chunk in kept {
            let payload_at = push_chunk(&mut body, &chunk.fourcc, chunk.data);
            if &chunk.fourcc == b"VP8X" {
                // `parse` refuses a VP8X shorter than its 10 bytes.
                let flags = &mut body[payload_at];
                *flags &= !(FLAG_ICC | FLAG_EXIF | FLAG_XMP);
                *flags |= self.kept_icc_flag();
                if rotated_exif.is_some() {
                    *flags |= FLAG_EXIF;
                }
            }
        }
        // The sizes come from a file of at most `MAX_FILE_BYTES`.
        riff_file(&body, body.len() as u32)
    }

    /// Where the `EXIF` chunk the stripped file keeps is: the first `EXIF`
    /// chunk, when it turns the picture. A reader uses the first one, so
    /// every other `EXIF` chunk goes.
    fn rotated_exif(&self) -> Option<usize> {
        self.chunks
            .iter()
            .position(|chunk| &chunk.fourcc == b"EXIF")
            .filter(|&at| exif::is_rotated(self.chunks[at].data))
    }

    /// The ICC flag a file written from this one carries: the source's own
    /// `VP8X` ICC bit, set or clear, when it has an `ICCP` chunk (every one
    /// is kept), so colour management is exactly as before; clear without
    /// one, and clear for a source without `VP8X`.
    fn kept_icc_flag(&self) -> u8 {
        let has_icc = self.chunks.iter().any(|chunk| &chunk.fourcc == b"ICCP");
        match self.chunks.first() {
            Some(first) if has_icc && &first.fourcc == b"VP8X" => first.data[0] & FLAG_ICC,
            _ => 0,
        }
    }
}

/// Append one chunk to `body`: FourCC, size, payload and the pad byte an odd
/// size needs. Returns where the payload starts. `data` is a chunk of a
/// parsed file, so its length fits the 32-bit size field.
fn push_chunk(body: &mut Vec<u8>, fourcc: &[u8; 4], data: &[u8]) -> usize {
    body.extend_from_slice(fourcc);
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let payload_at = body.len();
    body.extend_from_slice(data);
    if data.len() % 2 == 1 {
        body.push(0);
    }
    payload_at
}

/// `RIFF`, the size, then `body` (which starts with `WEBP`).
fn riff_file(body: &[u8], size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// A lossy result with the source's metadata, under exactly the rules of
/// [`WebpFile::stripped`]: every `ICCP` chunk of the source, byte for byte;
/// its first `EXIF` chunk only when that turns the picture; never `XMP ` or
/// an unknown chunk. Every odd chunk is padded and the RIFF size rewritten.
///
/// A still result (libwebp's simple `VP8 ` file, or its `VP8X`, `ALPH`,
/// `VP8 ` file) is written as `VP8X`, `ICCP`…, `ALPH`, the image, `EXIF`. A
/// `VP8X` is written when there is a profile, a rotation or alpha, and none
/// otherwise, so a result with nothing to add comes back as it is. Its
/// canvas is the image's own size, its reserved bits are zero, and its flags
/// match the chunks written: alpha for an `ALPH` chunk (or a `VP8L` image
/// that uses alpha), EXIF for the `EXIF` chunk, never XMP or animation, and
/// the ICC flag by the source's rule ([`WebpFile::kept_icc_flag`]).
///
/// An animated result (libwebp's `VP8X`, `ANIM`, `ANMF`… file) is written as
/// `VP8X`, `ICCP`…, `ANIM`, `ANMF`…, `EXIF`, the animation chunks byte for
/// byte. Its `VP8X` is the result's own (canvas, alpha flag and reserved
/// bits as libwebp wrote them) with the animation flag set, EXIF set for the
/// `EXIF` chunk, XMP clear and the ICC flag by the source's rule.
///
/// Refuses a result that is not a readable WebP file, a still image that
/// declares a zero width or height, and an animation without a `VP8X`.
pub(crate) fn with_metadata(encoded: &[u8], source: &WebpFile<'_>) -> Result<Vec<u8>, String> {
    let result = parse(encoded)
        .map_err(|reason| format!("the lossy result is not a readable WebP file ({reason})"))?;
    let metadata = Metadata {
        profiles: source
            .chunks
            .iter()
            .filter(|chunk| &chunk.fourcc == b"ICCP")
            .collect(),
        rotation: source.rotated_exif().map(|at| &source.chunks[at]),
        icc_flag: source.kept_icc_flag(),
    };
    let body = if result.kind == Kind::Animated {
        animation_body(&result, &metadata)?
    } else {
        still_body(&result, &metadata)?
    };
    let size = u32::try_from(body.len())
        .map_err(|_| "the lossy result is too large for a WebP file".to_owned())?;
    Ok(riff_file(&body, size))
}

/// What [`with_metadata`] carries over from the source.
struct Metadata<'s> {
    /// Every `ICCP` chunk, in order.
    profiles: Vec<&'s Chunk<'s>>,
    /// The first `EXIF` chunk, when it turns the picture.
    rotation: Option<&'s Chunk<'s>>,
    /// The ICC flag to write ([`WebpFile::kept_icc_flag`]).
    icc_flag: u8,
}

/// The chunks of a still result with the metadata, after `WEBP`.
fn still_body(result: &WebpFile<'_>, metadata: &Metadata<'_>) -> Result<Vec<u8>, String> {
    // A file that is not an animation has a still image chunk (`kind_of`).
    let image = result
        .chunks
        .iter()
        .find(|chunk| matches!(&chunk.fourcc, b"VP8 " | b"VP8L"))
        .ok_or_else(|| "the lossy result has no image".to_owned())?;
    let alph = (&image.fourcc == b"VP8 ")
        .then(|| result.chunks.iter().find(|chunk| &chunk.fourcc == b"ALPH"))
        .flatten();
    // `parse` read this header already, so it is there and well-formed.
    let (width, height) = declared_size(image).ok().flatten().unwrap_or_default();
    if width == 0 || height == 0 {
        return Err(format!(
            "the lossy result has an empty image ({width}×{height})"
        ));
    }
    let alpha = alph.is_some() || (&image.fourcc == b"VP8L" && vp8l_uses_alpha(image.data));

    let mut body = b"WEBP".to_vec();
    if alpha || !metadata.profiles.is_empty() || metadata.rotation.is_some() {
        let mut flags = metadata.icc_flag;
        if alpha {
            flags |= FLAG_ALPHA;
        }
        if metadata.rotation.is_some() {
            flags |= FLAG_EXIF;
        }
        let mut vp8x = vec![flags, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        push_chunk(&mut body, b"VP8X", &vp8x);
    }
    for chunk in metadata
        .profiles
        .iter()
        .copied()
        .chain(alph)
        .chain([image])
        .chain(metadata.rotation)
    {
        push_chunk(&mut body, &chunk.fourcc, chunk.data);
    }
    Ok(body)
}

/// The chunks of an animated result with the metadata, after `WEBP`.
fn animation_body(result: &WebpFile<'_>, metadata: &Metadata<'_>) -> Result<Vec<u8>, String> {
    let vp8x = result
        .chunks
        .first()
        .filter(|chunk| &chunk.fourcc == b"VP8X")
        .ok_or_else(|| "the lossy result is an animation without a VP8X chunk".to_owned())?;
    // `parse` refuses a VP8X shorter than its 10 bytes.
    let mut header = vp8x.data.to_vec();
    header[0] &= !(FLAG_ICC | FLAG_EXIF | FLAG_XMP);
    header[0] |= metadata.icc_flag | FLAG_ANIMATION;
    if metadata.rotation.is_some() {
        header[0] |= FLAG_EXIF;
    }

    let mut body = b"WEBP".to_vec();
    push_chunk(&mut body, b"VP8X", &header);
    let animation = result
        .chunks
        .iter()
        .filter(|chunk| matches!(&chunk.fourcc, b"ANIM" | b"ANMF"));
    for chunk in metadata
        .profiles
        .iter()
        .copied()
        .chain(animation)
        .chain(metadata.rotation)
    {
        push_chunk(&mut body, &chunk.fourcc, chunk.data);
    }
    Ok(body)
}

/// The length of an `ANMF` frame header: X and Y offsets, width and height,
/// duration (3 bytes each) and a flags byte.
const ANMF_HEADER_LEN: usize = 16;

/// What an animation's headers declare, read without decoding: the `VP8X`
/// canvas, the number of `ANMF` frames and the sum of their durations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnimationHeader {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub duration_ms: u64,
}

/// The canvas, frame count and total duration of an animation, from its
/// `VP8X` and `ANMF` headers; nothing is decoded. The total is the sum of
/// the 24-bit frame durations, which is what libwebp's animation decoder
/// counts up to. At most [`MAX_CHUNKS`] frames of at most 2^24 ms each, so
/// neither number can overflow.
///
/// Refuses a file without a `VP8X` (an animation needs one for its canvas)
/// and an `ANMF` chunk shorter than its frame header.
pub(crate) fn animation_header(file: &WebpFile<'_>) -> Result<AnimationHeader, String> {
    let vp8x = file
        .chunks
        .first()
        .filter(|chunk| &chunk.fourcc == b"VP8X")
        .ok_or_else(|| unreadable("the animation has no VP8X chunk"))?;
    // `parse` read this header already, so it is there and well-formed.
    let (width, height) = declared_size(vp8x)?.unwrap_or_default();
    let mut frames = 0;
    let mut duration_ms = 0;
    for frame in file.chunks.iter().filter(|chunk| &chunk.fourcc == b"ANMF") {
        let Some(&[.., d0, d1, d2, _flags]) = frame.data.first_chunk::<ANMF_HEADER_LEN>() else {
            return Err(unreadable(&format!(
                "an ANMF chunk is shorter than its {ANMF_HEADER_LEN}-byte header"
            )));
        };
        frames += 1;
        duration_ms += u64::from(u32::from_le_bytes([d0, d1, d2, 0]));
    }
    Ok(AnimationHeader {
        width,
        height,
        frames,
        duration_ms,
    })
}

/// Whether a `VP8L` image says it uses alpha: bit 28 of its header, after
/// the signature byte. `parse` checked the header is there.
fn vp8l_uses_alpha(data: &[u8]) -> bool {
    data.get(1..5)
        .and_then(|bits| bits.try_into().ok())
        .is_some_and(|bits: [u8; 4]| u32::from_le_bytes(bits) >> 28 & 1 == 1)
}

/// The error for a file whose RIFF container is broken.
fn unreadable(why: &str) -> String {
    format!("could not read the WebP data: {why}")
}

/// Check that `bytes` is WebP data, walk its chunks and check the size of
/// the image it declares, in that order.
///
/// The walk stops at the RIFF size: bytes after it are ignored, and a chunk
/// that runs past it is refused. Every offset is computed with checked
/// arithmetic, so a declared size of 4 GB is an error, not a panic or an
/// allocation. A `VP8X` must be the first chunk and may occur once, as in the
/// container specification; a file with no image chunk is refused.
///
/// The size check covers the `VP8X` canvas and, as well, the header of every
/// `VP8L` and `VP8 ` chunk, against [`crate::MAX_PIXELS`]. It comes before
/// the check for an image chunk, so a canvas over the limit is "too large"
/// even when the file holds no image.
///
/// The errors are messages for the user: "not a WebP file (…)" when the
/// content is not WebP data, `check_pixels`'s text when the image is too
/// large, and "could not read the WebP data: …" otherwise.
pub(crate) fn parse(bytes: &[u8]) -> Result<WebpFile<'_>, String> {
    let body = riff_body(bytes)?;
    let chunks = walk(body)?;
    for chunk in &chunks {
        if let Some((width, height)) = declared_size(chunk)? {
            check_pixels(width, height)?;
        }
    }
    let kind = kind_of(&chunks)?;
    Ok(WebpFile { chunks, kind })
}

/// The chunks of the file: the bytes between the `RIFF` header and the end
/// the RIFF size gives. Refuses what is not `RIFF`…`WEBP`, and a size that is
/// too small for the form type or runs past the end of the file.
fn riff_body(bytes: &[u8]) -> Result<&[u8], String> {
    let Some(header) = bytes.first_chunk::<RIFF_HEADER>() else {
        return Err(NOT_WEBP.to_owned());
    };
    if &header[..4] != b"RIFF" || &header[8..] != b"WEBP" {
        return Err(NOT_WEBP.to_owned());
    }
    let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    // The size counts everything after the first 8 bytes.
    match size.checked_add(8) {
        Some(end) if (RIFF_HEADER..=bytes.len()).contains(&end) => Ok(&bytes[RIFF_HEADER..end]),
        _ => Err(unreadable(
            "the RIFF size is too small, or runs past the end of the file",
        )),
    }
}

/// Every chunk of `body`, in order: a header of FourCC and little-endian
/// size, the payload, and one zero byte when the size is odd. A final odd
/// chunk may lack its padding byte; some writers leave it out.
fn walk(body: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    let mut chunks = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let header = at.checked_add(8).and_then(|end| body.get(at..end));
        let Some(&[f0, f1, f2, f3, s0, s1, s2, s3]) = header else {
            return Err(unreadable("a chunk header is cut short"));
        };
        let fourcc = [f0, f1, f2, f3];
        let size = u32::from_le_bytes([s0, s1, s2, s3]) as usize;
        let start = at + 8;
        let Some(end) = start.checked_add(size).filter(|&end| end <= body.len()) else {
            return Err(unreadable(&format!(
                "the {} chunk runs past the end of the file",
                name_of(&fourcc)
            )));
        };
        if chunks.len() == MAX_CHUNKS {
            return Err(unreadable(&format!("it has more than {MAX_CHUNKS} chunks")));
        }
        if &fourcc == b"VP8X" {
            if chunks.first().is_some_and(|c: &Chunk| &c.fourcc == b"VP8X") {
                return Err(unreadable("it has more than one VP8X chunk"));
            }
            if !chunks.is_empty() {
                return Err(unreadable("the VP8X chunk is not the first chunk"));
            }
        }
        chunks.push(Chunk {
            fourcc,
            data: &body[start..end],
        });
        // The pad byte is skipped; at most it is the one missing at the end.
        at = end.saturating_add(size & 1).min(body.len());
    }
    Ok(chunks)
}

/// The FourCC as text for a message; it can be any four bytes.
fn name_of(fourcc: &[u8; 4]) -> String {
    String::from_utf8_lossy(fourcc).trim_end().to_owned()
}

/// The image size a chunk declares in its header, for `VP8X` (the canvas),
/// `VP8L` and `VP8 `; `None` for every other chunk.
fn declared_size(chunk: &Chunk<'_>) -> Result<Option<(u32, u32)>, String> {
    match &chunk.fourcc {
        // Flags, 3 reserved bytes, then width-1 and height-1, 24 bits each.
        b"VP8X" => {
            let &[_, _, _, _, w0, w1, w2, h0, h1, h2, ..] = chunk.data else {
                return Err(unreadable(&format!(
                    "the VP8X chunk is shorter than its {VP8X_LEN}-byte header"
                )));
            };
            let width = u32::from_le_bytes([w0, w1, w2, 0]) + 1;
            let height = u32::from_le_bytes([h0, h1, h2, 0]) + 1;
            Ok(Some((width, height)))
        }
        // The signature 0x2F, then width-1 and height-1 in 14 bits each.
        b"VP8L" => {
            let &[signature, b0, b1, b2, b3, ..] = chunk.data else {
                return Err(unreadable(
                    "the VP8L chunk is shorter than its 5-byte header",
                ));
            };
            if signature != 0x2F {
                return Err(unreadable("the VP8L chunk does not start with 0x2F"));
            }
            let bits = u32::from_le_bytes([b0, b1, b2, b3]);
            Ok(Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)))
        }
        // The 3-byte frame tag, the start code, then width and height in 14
        // bits each (the top two bits are a scale).
        b"VP8 " => {
            let &[_, _, _, 0x9D, 0x01, 0x2A, w0, w1, h0, h1, ..] = chunk.data else {
                return Err(unreadable(
                    "the VP8 chunk is too short, or lacks its start code",
                ));
            };
            let width = u32::from(u16::from_le_bytes([w0, w1]) & 0x3FFF);
            let height = u32::from(u16::from_le_bytes([h0, h1]) & 0x3FFF);
            Ok(Some((width, height)))
        }
        _ => Ok(None),
    }
}

/// What the chunks make: an animation (`ANIM` and a frame), else the still
/// image of the first `VP8L` or `VP8 ` chunk. No image chunk is an error.
fn kind_of(chunks: &[Chunk<'_>]) -> Result<Kind, String> {
    let has = |fourcc: &[u8; 4]| chunks.iter().any(|c| &c.fourcc == fourcc);
    if has(b"ANIM") && has(b"ANMF") {
        return Ok(Kind::Animated);
    }
    match chunks
        .iter()
        .find(|c| matches!(&c.fourcc, b"VP8L" | b"VP8 "))
    {
        Some(c) if &c.fourcc == b"VP8L" => Ok(Kind::Lossless),
        Some(_) => Ok(Kind::Lossy),
        None => Err(unreadable("it has no image chunk")),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Generous: a refusal takes microseconds; a walk that trusts a declared
    /// size takes far longer or panics.
    const QUICK: Duration = Duration::from_secs(5);

    const NOT_WEBP: &str = "not a WebP file (the content is not WebP data)";
    const UNREADABLE: &str = "could not read the WebP data:";

    // --- fixtures -----------------------------------------------------------

    /// One chunk: FourCC, size, payload, and the zero pad byte of an odd size.
    pub(in crate::webp) fn chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = raw_chunk(fourcc, data.len() as u32, data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// A chunk header declaring `size`, followed by `data` as it is, with no
    /// padding: for chunks whose declared size is wrong on purpose.
    pub(in crate::webp) fn raw_chunk(fourcc: &[u8; 4], size: u32, data: &[u8]) -> Vec<u8> {
        let mut out = fourcc.to_vec();
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// A RIFF header whose size is right for `body`, then `body`.
    fn riff_of(body: &[u8]) -> Vec<u8> {
        riff_declaring(4 + body.len() as u32, body)
    }

    /// A RIFF header with the given size field, `WEBP`, then `body`.
    pub(in crate::webp) fn riff_declaring(size: u32, body: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(body);
        out
    }

    pub(in crate::webp) fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        riff_of(&chunks.concat())
    }

    /// A `VP8X` chunk for a `w`x`h` canvas (stored as width-1 and height-1,
    /// 24 bits each).
    pub(in crate::webp) fn vp8x(flags: u8, w: u32, h: u32) -> Vec<u8> {
        let mut data = vec![flags, 0, 0, 0];
        data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        chunk(b"VP8X", &data)
    }

    /// A `VP8L` payload that declares `w`x`h`, with a few bytes of "bitstream"
    /// behind the header (the walk never decodes it).
    pub(in crate::webp) fn vp8l_payload(w: u32, h: u32) -> Vec<u8> {
        let bits = (w - 1) | ((h - 1) << 14);
        let mut data = vec![0x2F];
        data.extend_from_slice(&bits.to_le_bytes());
        data.extend_from_slice(&[0; 5]);
        data
    }

    pub(in crate::webp) fn vp8l(w: u32, h: u32) -> Vec<u8> {
        chunk(b"VP8L", &vp8l_payload(w, h))
    }

    /// A `VP8 ` payload that declares `w`x`h`: the key frame tag, the start
    /// code, the two sizes, and a few bytes of "bitstream".
    pub(in crate::webp) fn vp8_payload(w: u16, h: u16) -> Vec<u8> {
        let mut data = vec![0x30, 0x01, 0x00, 0x9D, 0x01, 0x2A];
        data.extend_from_slice(&w.to_le_bytes());
        data.extend_from_slice(&h.to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        data
    }

    pub(in crate::webp) fn vp8(w: u16, h: u16) -> Vec<u8> {
        chunk(b"VP8 ", &vp8_payload(w, h))
    }

    /// An `ANMF` frame: the 16-byte frame header and a few bytes of frame data.
    fn anmf() -> Vec<u8> {
        chunk(b"ANMF", &[0; 24])
    }

    /// An `ANMF` chunk of only its 16-byte frame header: at 0,0, `w`x`h`,
    /// shown for `duration_ms`, with every flag bit set (so a reader that
    /// takes 32 bits for the 24-bit duration reads a different number).
    pub(in crate::webp) fn anmf_header(w: u32, h: u32, duration_ms: u32) -> Vec<u8> {
        let mut data = vec![0u8; 6];
        data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&duration_ms.to_le_bytes()[..3]);
        data.push(0xFF);
        chunk(b"ANMF", &data)
    }

    /// `ANIM`: background colour and loop count.
    fn anim() -> Vec<u8> {
        chunk(b"ANIM", &[0; 6])
    }

    /// A real lossless WebP from `image`'s encoder.
    fn real_lossless() -> Vec<u8> {
        let pixels: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(&pixels, 16, 16, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// `parse`, which must come back quickly whatever the input declares.
    fn parse_quickly(bytes: &[u8]) -> Result<WebpFile<'_>, String> {
        let started = Instant::now();
        let result = parse(bytes);
        assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
        result
    }

    /// The error of a file the walk must refuse as unreadable.
    fn unreadable(bytes: &[u8]) -> String {
        let err = parse_quickly(bytes).unwrap_err();
        assert!(err.starts_with(UNREADABLE), "{err}");
        err
    }

    fn fourccs(file: &WebpFile<'_>) -> Vec<String> {
        file.chunks
            .iter()
            .map(|c| String::from_utf8_lossy(&c.fourcc).into_owned())
            .collect()
    }

    // --- the content check ----------------------------------------------------

    #[test]
    fn the_fixture_helpers_write_a_riff_size_that_covers_the_file() {
        // Proof that the hand-made files say what the tests rely on.
        let file = riff(&[vp8x(0, 1, 1), vp8l(1, 1)]);
        assert_eq!(
            u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize,
            file.len() - 8
        );
        assert_eq!(&file[8..12], b"WEBP");
    }

    #[test]
    fn refuses_what_is_not_webp_data() {
        let png = [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&36u32.to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        let mut not_riff = riff(&[vp8l(1, 1)]);
        not_riff[0] = b'X';
        let mut not_webp_form = riff(&[vp8l(1, 1)]);
        not_webp_form[11] = b'X';
        let cases: [&[u8]; 7] = [
            &png,
            &wave,
            &[],
            b"RIFF",
            // The header cut one byte short of its twelve.
            b"RIFF\x04\x00\x00\x00WEB",
            &not_riff,
            &not_webp_form,
        ];
        for case in cases {
            assert_eq!(parse_quickly(case).unwrap_err(), NOT_WEBP, "{case:?}");
        }
    }

    // --- what kind of file it is ------------------------------------------------

    #[test]
    fn parses_a_simple_lossless_file_as_lossless() {
        let bytes = real_lossless();
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.kind, Kind::Lossless);
        assert_eq!(fourccs(&file), ["VP8L"]);
        assert_eq!(file.chunks[0].data[0], 0x2F);
    }

    #[test]
    fn parses_a_lossy_file_as_lossy() {
        let bytes = riff(&[vp8(640, 480)]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.kind, Kind::Lossy);
        assert_ne!(file.kind, Kind::Lossless);
        assert_eq!(fourccs(&file), ["VP8 "]);
    }

    #[test]
    fn parses_a_lossy_file_with_alpha_as_lossy() {
        let bytes = riff(&[vp8x(0x10, 64, 64), chunk(b"ALPH", &[0; 12]), vp8(64, 64)]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.kind, Kind::Lossy);
        assert_eq!(fourccs(&file), ["VP8X", "ALPH", "VP8 "]);
    }

    #[test]
    fn parses_an_extended_lossless_file_as_lossless() {
        let bytes = riff(&[vp8x(0, 32, 32), vp8l(32, 32)]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.kind, Kind::Lossless);
        assert_eq!(fourccs(&file), ["VP8X", "VP8L"]);
    }

    #[test]
    fn parses_an_animated_file_as_animated() {
        let bytes = riff(&[vp8x(0x02, 100, 100), anim(), anmf(), anmf()]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.kind, Kind::Animated);
        assert_eq!(fourccs(&file), ["VP8X", "ANIM", "ANMF", "ANMF"]);
    }

    #[test]
    fn does_not_call_the_same_file_without_anim_and_anmf_animated() {
        // The animation flag alone does not make a still image animated.
        let flagged_still = riff(&[vp8x(0x02, 100, 100), vp8l(100, 100)]);
        assert_eq!(parse_quickly(&flagged_still).unwrap().kind, Kind::Lossless);

        // An ANIM chunk without frames does not either.
        let no_frames = riff(&[vp8x(0x02, 100, 100), anim(), vp8l(100, 100)]);
        assert_eq!(parse_quickly(&no_frames).unwrap().kind, Kind::Lossless);

        // Frames without ANIM are no animation and no still image.
        let no_anim = riff(&[vp8x(0x02, 100, 100), anmf(), anmf()]);
        assert!(unreadable(&no_anim).contains("no image chunk"));
    }

    // --- VP8X --------------------------------------------------------------------

    #[test]
    fn parses_a_vp8x_that_comes_first_exactly_once() {
        let bytes = riff(&[vp8x(0x20, 8, 8), chunk(b"ICCP", &[1, 2, 3, 4]), vp8l(8, 8)]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(fourccs(&file), ["VP8X", "ICCP", "VP8L"]);
        assert_eq!(file.chunks[0].data.len(), 10);
    }

    #[test]
    fn refuses_a_vp8x_that_is_not_first() {
        let err = unreadable(&riff(&[vp8l(8, 8), vp8x(0, 8, 8)]));
        assert!(err.contains("VP8X") && err.contains("first"), "{err}");
        let err = unreadable(&riff(&[chunk(b"ICCP", &[0; 4]), vp8x(0, 8, 8), vp8l(8, 8)]));
        assert!(err.contains("VP8X") && err.contains("first"), "{err}");
    }

    #[test]
    fn refuses_a_second_vp8x() {
        let err = unreadable(&riff(&[vp8x(0, 8, 8), vp8x(0, 8, 8), vp8l(8, 8)]));
        assert!(err.contains("more than one VP8X"), "{err}");
    }

    #[test]
    fn refuses_a_vp8x_shorter_than_its_header() {
        let short = chunk(b"VP8X", &[0; 9]);
        let err = unreadable(&riff(&[short, vp8l(8, 8)]));
        assert!(err.contains("VP8X"), "{err}");
    }

    #[test]
    fn refuses_a_file_without_an_image_chunk() {
        let exif_only = riff(&[vp8x(0x08, 8, 8), chunk(b"EXIF", &[0; 6])]);
        assert!(unreadable(&exif_only).contains("no image chunk"));
        // Not even a VP8X.
        assert!(unreadable(&riff(&[])).contains("no image chunk"));
    }

    // --- the chunk walk -------------------------------------------------------------

    #[test]
    fn refuses_a_chunk_past_the_end() {
        // A declared size of four gigabytes, in a RIFF that is otherwise right.
        let huge = raw_chunk(b"VP8L", u32::MAX, &vp8l_payload(8, 8));
        unreadable(&riff_of(&huge));
        let huge_after_vp8x = [vp8x(0, 8, 8), raw_chunk(b"EXIF", u32::MAX, &[0; 6])].concat();
        unreadable(&riff_of(&huge_after_vp8x));
        // Just one byte past the end.
        let payload = vp8l_payload(8, 8);
        let one_over = raw_chunk(b"VP8L", payload.len() as u32 + 1, &payload);
        unreadable(&riff_of(&one_over));
    }

    #[test]
    fn refuses_a_riff_size_larger_than_the_file() {
        let good = riff(&[vp8x(0, 8, 8), vp8l(8, 8)]);
        let body = &good[12..];
        for extra in [1u32, 1000, u32::MAX - 4 - body.len() as u32] {
            let bytes = riff_declaring(4 + body.len() as u32 + extra, body);
            unreadable(&bytes);
        }
        // The size field alone is at its largest.
        unreadable(&riff_declaring(u32::MAX, body));
        // And the exact size is accepted.
        assert!(parse_quickly(&good).is_ok());
    }

    #[test]
    fn refuses_a_riff_size_too_small_for_the_form_type() {
        for size in 0..4u32 {
            unreadable(&riff_declaring(size, &vp8l(8, 8)));
        }
    }

    #[test]
    fn refuses_a_chunk_that_runs_past_the_riff_size_inside_the_file() {
        let mut bytes = riff(&[vp8x(0, 8, 8), vp8l(8, 8)]);
        let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        // The RIFF ends 4 bytes early; the file does not.
        bytes[4..8].copy_from_slice(&(size - 4).to_le_bytes());
        unreadable(&bytes);

        // The same for a chunk of odd size whose last payload byte is cut off.
        let odd = riff(&[vp8x(0, 8, 8), chunk(b"VP8L", &vp8l_payload(8, 8)[..9])]);
        let mut cut = odd.clone();
        let size = u32::from_le_bytes(cut[4..8].try_into().unwrap());
        cut[4..8].copy_from_slice(&(size - 2).to_le_bytes());
        unreadable(&cut);
        assert!(parse_quickly(&odd).is_ok());
    }

    #[test]
    fn refuses_a_chunk_header_cut_short() {
        for left in 1..8 {
            let mut body = [vp8x(0, 8, 8), vp8l(8, 8)].concat();
            body.extend_from_slice(&b"JUNKJUNK"[..left]);
            unreadable(&riff_of(&body));
        }
    }

    #[test]
    fn ignores_bytes_after_the_riff_size() {
        let clean = riff(&[vp8x(0, 8, 8), vp8l(8, 8)]);
        let mut with_tail = clean.clone();
        // A whole chunk after the RIFF, and then bytes that are no chunk.
        with_tail.extend(chunk(b"EXIF", &[0; 6]));
        with_tail.extend_from_slice(b"junk");
        let file = parse_quickly(&with_tail).unwrap();
        assert_eq!(fourccs(&file), ["VP8X", "VP8L"]);
        assert_eq!(file.kind, Kind::Lossless);
    }

    #[test]
    fn skips_the_padding_byte_of_an_odd_sized_chunk() {
        let bytes = riff(&[vp8x(0x20, 8, 8), chunk(b"ICCP", &[1, 2, 3]), vp8l(8, 8)]);
        let file = parse_quickly(&bytes).unwrap();
        // The chunk after the odd one is found, and the pad is not its data.
        assert_eq!(fourccs(&file), ["VP8X", "ICCP", "VP8L"]);
        assert_eq!(file.chunks[1].data, [1, 2, 3]);
    }

    #[test]
    fn accepts_a_final_odd_sized_chunk_without_its_padding_byte() {
        // Some writers leave the last pad out of the RIFF size.
        let payload = vp8l_payload(8, 8);
        assert_eq!(payload.len() % 2, 0);
        let odd = &payload[..9];
        let body = [vp8x(0, 8, 8), raw_chunk(b"VP8L", 9, odd)].concat();
        let bytes = riff_of(&body);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.chunks[1].data, odd);
    }

    #[test]
    fn accepts_exactly_the_chunk_limit() {
        let mut body = vp8l(8, 8);
        for _ in 1..MAX_CHUNKS {
            body.extend(chunk(b"JUNK", &[]));
        }
        let bytes = riff_of(&body);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(file.chunks.len(), MAX_CHUNKS);
    }

    #[test]
    fn refuses_more_chunks_than_the_limit() {
        let mut body = vp8l(8, 8);
        for _ in 0..MAX_CHUNKS {
            body.extend(chunk(b"JUNK", &[]));
        }
        let err = unreadable(&riff_of(&body));
        assert!(err.contains("chunks"), "{err}");
    }

    // --- the image headers ---------------------------------------------------------

    #[test]
    fn refuses_a_vp8l_shorter_than_its_header() {
        for len in 0..5 {
            let short = chunk(b"VP8L", &vp8l_payload(8, 8)[..len]);
            let err = unreadable(&riff(&[short]));
            assert!(err.contains("VP8L"), "{err}");
        }
    }

    #[test]
    fn refuses_a_vp8l_without_its_signature_byte() {
        let mut payload = vp8l_payload(8, 8);
        payload[0] = 0x2E;
        let err = unreadable(&riff(&[chunk(b"VP8L", &payload)]));
        assert!(err.contains("VP8L"), "{err}");
    }

    #[test]
    fn refuses_a_vp8_shorter_than_its_header() {
        for len in 0..10 {
            let short = chunk(b"VP8 ", &vp8_payload(8, 8)[..len]);
            let err = unreadable(&riff(&[short]));
            assert!(err.contains("VP8"), "{err}");
        }
    }

    #[test]
    fn refuses_a_vp8_without_its_start_code() {
        let mut payload = vp8_payload(8, 8);
        payload[3] = 0x9C;
        let err = unreadable(&riff(&[chunk(b"VP8 ", &payload)]));
        assert!(err.contains("start code"), "{err}");
    }

    // --- the pixel limit --------------------------------------------------------------

    #[test]
    fn refuses_a_canvas_over_the_pixel_limit() {
        // A VP8X alone, so only the size can be what refuses it.
        let bomb = riff(&[vp8x(0, 20_000, 20_000)]);
        assert!(bomb.len() < 64);
        assert_eq!(
            parse_quickly(&bomb).unwrap_err(),
            "image too large: 20000×20000 exceeds 100 megapixels"
        );
        // The largest canvas the 24-bit fields hold.
        let largest = riff(&[vp8x(0, 1 << 24, 1 << 24)]);
        assert_eq!(
            parse_quickly(&largest).unwrap_err(),
            "image too large: 16777216×16777216 exceeds 100 megapixels"
        );
        // One row over the limit, next to an image header within it.
        let over = riff(&[vp8x(0, 10_000, 10_001), vp8l(1, 1)]);
        assert!(
            parse_quickly(&over)
                .unwrap_err()
                .starts_with("image too large")
        );
    }

    #[test]
    fn refuses_a_canvas_at_the_limit_with_no_image_chunk_as_unreadable_not_too_large() {
        let err = unreadable(&riff(&[vp8x(0, 10_000, 10_000)]));
        assert!(!err.contains("too large"), "{err}");
    }

    #[test]
    fn parses_a_vp8x_canvas_at_the_pixel_limit() {
        let bytes = riff(&[vp8x(0, 10_000, 10_000), vp8l(1, 1)]);
        assert_eq!(parse_quickly(&bytes).unwrap().kind, Kind::Lossless);
    }

    #[test]
    fn parses_a_vp8l_and_a_vp8_header_within_the_limit() {
        assert!(parse_quickly(&riff(&[vp8l(10_000, 10_000)])).is_ok());
        assert!(parse_quickly(&riff(&[vp8(10_000, 10_000)])).is_ok());
        assert!(parse_quickly(&riff(&[vp8l(16_384, 1)])).is_ok());
    }

    #[test]
    fn refuses_a_vp8l_header_over_the_limit_under_a_small_canvas() {
        // The canvas passes; the image header declares the 14-bit largest.
        let bytes = riff(&[vp8x(0, 100, 100), vp8l(16_384, 16_384)]);
        assert_eq!(
            parse_quickly(&bytes).unwrap_err(),
            "image too large: 16384×16384 exceeds 100 megapixels"
        );
        let alone = riff(&[vp8l(10_001, 10_000)]);
        assert!(
            parse_quickly(&alone)
                .unwrap_err()
                .starts_with("image too large")
        );
    }

    #[test]
    fn refuses_a_vp8_header_over_the_limit() {
        let bytes = riff(&[vp8(16_383, 16_383)]);
        assert_eq!(
            parse_quickly(&bytes).unwrap_err(),
            "image too large: 16383×16383 exceeds 100 megapixels"
        );
    }

    #[test]
    fn refuses_a_canvas_over_the_limit_over_a_small_vp8l_header() {
        // Both sizes are checked: a small image header does not excuse the canvas.
        let bytes = riff(&[vp8x(0, 20_000, 20_000), vp8l(1, 1)]);
        assert_eq!(
            parse_quickly(&bytes).unwrap_err(),
            "image too large: 20000×20000 exceeds 100 megapixels"
        );
    }

    // --- stripped: what is kept ---------------------------------------------------

    use crate::webp::fixtures as fx;

    const SIZE: u32 = 16;
    /// An ICC profile of even size, and one of odd size.
    const ICC_EVEN: &[u8] = &[0x11; 128];
    const ICC_ODD: &[u8] = &[0x22; 129];

    fn pixels(alpha: bool) -> Vec<u8> {
        fx::photo_rgba(SIZE, SIZE, alpha)
    }

    fn lossless_file(alpha: bool) -> Vec<u8> {
        fx::lossless(&pixels(alpha), SIZE, SIZE)
    }

    fn stripped_of(bytes: &[u8]) -> Vec<u8> {
        parse(bytes).unwrap().stripped()
    }

    fn flags(file: &[u8]) -> u8 {
        file[fx::VP8X_FLAGS_AT]
    }

    /// The stripped file is a whole, well-formed file: the RIFF size covers
    /// exactly the bytes, every chunk is padded (so the length is even) and
    /// it parses again.
    fn assert_well_formed(file: &[u8]) {
        let size = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
        assert_eq!(size, file.len() - 8, "the RIFF size");
        assert_eq!(file.len() % 2, 0, "every chunk is padded");
        let parsed = parse(file).unwrap();
        assert_eq!(parsed.chunks.len(), fx::chunks_of(file).len());
    }

    #[test]
    fn removes_exif_and_xmp_and_keeps_icc_byte_identical() {
        let base = lossless_file(true);
        let input = fx::with_chunks(
            &base,
            &[
                (*b"ICCP", ICC_EVEN),
                (*b"EXIF", b"Exif\0\0 a camera"),
                (*b"XMP ", b"<x:xmpmeta/>"),
            ],
        );
        // The fixture holds what the test is about.
        assert_eq!(
            flags(&input) & (fx::FLAG_ICC | fx::FLAG_EXIF | fx::FLAG_XMP),
            fx::FLAG_ICC | fx::FLAG_EXIF | fx::FLAG_XMP
        );

        let out = stripped_of(&input);

        assert_well_formed(&out);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8L"]);
        assert_eq!(fx::chunk_payload(&out, b"ICCP").as_deref(), Some(ICC_EVEN));
        assert_eq!(
            fx::chunk_payload(&out, b"VP8L"),
            fx::chunk_payload(&input, b"VP8L")
        );
        // The flags match the chunks left; alpha is as it was.
        assert_eq!(flags(&out), (flags(&input) & fx::FLAG_ALPHA) | fx::FLAG_ICC);
        // The rest of the VP8X (reserved bytes, canvas) is as it was.
        assert_eq!(
            fx::chunk_payload(&out, b"VP8X").unwrap()[1..],
            fx::chunk_payload(&input, b"VP8X").unwrap()[1..]
        );
        assert!(out.len() < input.len());
        assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&input));
    }

    #[test]
    fn adds_nothing_to_a_file_without_metadata() {
        // An ICC profile only: no chunk and no flag appears, the profile stays.
        let with_icc = fx::with_chunks(&lossless_file(false), &[(*b"ICCP", ICC_EVEN)]);
        let out = stripped_of(&with_icc);
        assert_eq!(out, with_icc);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8L"]);
        // The flags are the input's own: ICC (and alpha, which the encoder
        // sets for any RGBA image), and no EXIF or XMP gained.
        assert_eq!(flags(&out), flags(&with_icc));
        assert_eq!(flags(&out) & fx::FLAG_ICC, fx::FLAG_ICC);
        assert_eq!(flags(&out) & (fx::FLAG_EXIF | fx::FLAG_XMP), 0);

        // An extended file with no flags and no metadata.
        let image = fx::chunks_of(&lossless_file(false)).remove(0);
        let plain = fx::from_chunks(&[(*b"VP8X", fx::vp8x_payload(0, SIZE, SIZE)), image]);
        assert_eq!(stripped_of(&plain), plain);
        assert_eq!(flags(&plain), 0);
    }

    #[test]
    fn keeps_a_simple_file_simple() {
        let simple = lossless_file(false);
        assert_eq!(fx::chunk_names(&simple), ["VP8L"]);
        let out = stripped_of(&simple);
        assert_eq!(out, simple);
        assert_eq!(fx::chunk_names(&out), ["VP8L"]);

        // The same image in an extended file keeps its VP8X.
        let extended = fx::with_chunks(&simple, &[(*b"EXIF", b"exif")]);
        assert_eq!(fx::chunk_names(&stripped_of(&extended)), ["VP8X", "VP8L"]);
    }

    #[test]
    fn drops_unknown_chunks() {
        let input = fx::with_chunks(
            &lossless_file(false),
            &[(*b"ABCD", b"unknown"), (*b"JUNK", b"")],
        );
        let out = stripped_of(&input);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L"]);
        assert_well_formed(&out);
        // They set no flag, so the VP8X is unchanged.
        assert_eq!(flags(&out), flags(&input));
    }

    #[test]
    fn keeps_every_kept_chunk_in_its_original_order() {
        // Distinct payloads, and the dropped chunks between the kept ones.
        let vp8x = fx::vp8x_payload(fx::FLAG_ANIMATION | fx::FLAG_ICC, SIZE, SIZE);
        let input = fx::from_chunks(&[
            (*b"VP8X", vp8x),
            (*b"ICCP", vec![1; 6]),
            (*b"ABCD", vec![2; 6]),
            (*b"ANIM", vec![3; 6]),
            (*b"ANMF", vec![4; 24]),
            (*b"EXIF", vec![5; 6]),
            (*b"ANMF", vec![6; 24]),
            (*b"XMP ", vec![7; 6]),
        ]);
        let out = stripped_of(&input);
        assert_eq!(
            fx::chunks_of(&out)[1..],
            [
                (*b"ICCP", vec![1; 6]),
                (*b"ANIM", vec![3; 6]),
                (*b"ANMF", vec![4; 24]),
                (*b"ANMF", vec![6; 24]),
            ]
        );
        assert_well_formed(&out);
    }

    #[test]
    fn keeps_every_anmf_chunk_of_an_animated_file() {
        let frames: Vec<Vec<u8>> = (0u8..3)
            .map(|k| {
                let rgba: Vec<u8> = pixels(true)
                    .iter()
                    .map(|b| b.wrapping_add(k * 40))
                    .collect();
                fx::lossless(&rgba, SIZE, SIZE)
            })
            .collect();
        let animated = fx::animated(&frames, SIZE, SIZE);
        let input = fx::with_chunks(
            &animated,
            &[
                (*b"EXIF", b"exif data"),
                (*b"XMP ", b"<xmp/>"),
                (*b"ABCD", b"x"),
            ],
        );

        let out = stripped_of(&input);

        assert_well_formed(&out);
        assert_eq!(
            fx::chunk_names(&out),
            ["VP8X", "ANIM", "ANMF", "ANMF", "ANMF"]
        );
        let anmf = |file: &[u8]| -> Vec<Vec<u8>> {
            fx::chunks_of(file)
                .into_iter()
                .filter(|(name, _)| name == b"ANMF")
                .map(|(_, data)| data)
                .collect()
        };
        assert_eq!(anmf(&out), anmf(&input));
        assert_eq!(anmf(&out).len(), 3);
        assert_eq!(
            flags(&out) & (fx::FLAG_EXIF | fx::FLAG_XMP | fx::FLAG_ICC),
            0
        );
        assert_eq!(flags(&out) & fx::FLAG_ANIMATION, fx::FLAG_ANIMATION);
        let decoded = fx::decode_frames(&out);
        assert_eq!(decoded, fx::decode_frames(&input));
        assert_ne!(decoded[0].0, decoded[1].0, "the frames differ");
    }

    #[test]
    fn copies_an_animated_file_without_metadata_as_it_is() {
        let frames = [lossless_file(true), lossless_file(false)];
        let animated = fx::animated(&frames, SIZE, SIZE);
        assert_eq!(stripped_of(&animated), animated);
    }

    #[test]
    fn keeps_vp8_and_alph_of_a_lossy_file_with_alpha_byte_identical() {
        let lossy = fx::lossy(&pixels(true), SIZE, SIZE, 75.0);
        assert_eq!(fx::chunk_names(&lossy), ["VP8X", "ALPH", "VP8 "]);
        let input = fx::with_chunks(&lossy, &[(*b"ICCP", ICC_EVEN), (*b"EXIF", &[4u8; 2_000])]);

        let out = stripped_of(&input);

        assert_well_formed(&out);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "ALPH", "VP8 "]);
        for fourcc in [b"ALPH", b"VP8 "] {
            assert_eq!(
                fx::chunk_payload(&out, fourcc),
                fx::chunk_payload(&lossy, fourcc)
            );
        }
        assert_eq!(flags(&out) & fx::FLAG_ALPHA, fx::FLAG_ALPHA);
        assert!(out.len() <= input.len());
        assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&input));
    }

    #[test]
    fn does_not_make_a_lossy_file_larger() {
        for alpha in [false, true] {
            let lossy = fx::lossy(&pixels(alpha), SIZE, SIZE, 75.0);
            assert_eq!(stripped_of(&lossy), lossy, "alpha: {alpha}");
        }
    }

    // --- stripped: padding, the tail and the flags ----------------------------------

    #[test]
    fn keeps_the_padding_byte_of_an_odd_sized_chunk() {
        let base = lossless_file(false);
        let input = fx::with_chunks(&base, &[(*b"ICCP", ICC_ODD), (*b"EXIF", b"odd")]);
        // The pad bytes are really in the fixture.
        let image_len = fx::chunk_payload(&base, b"VP8L").unwrap().len();
        assert_eq!(
            input.len(),
            12 + (8 + 10) + (8 + ICC_ODD.len() + 1) + (8 + image_len + image_len % 2) + (8 + 3 + 1)
        );

        let out = stripped_of(&input);

        // VP8X, the odd ICCP with its pad, and the image: nothing else.
        assert_eq!(
            out.len(),
            12 + (8 + 10) + (8 + ICC_ODD.len() + 1) + (8 + image_len + image_len % 2)
        );
        assert_well_formed(&out);
        assert_eq!(fx::chunk_payload(&out, b"ICCP").as_deref(), Some(ICC_ODD));
        assert_eq!(
            fx::chunk_payload(&out, b"VP8L"),
            fx::chunk_payload(&base, b"VP8L")
        );
        assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&base));
    }

    #[test]
    fn writes_no_padding_byte_after_an_even_sized_chunk() {
        let base = lossless_file(false);
        let input = fx::with_chunks(&base, &[(*b"ICCP", ICC_EVEN)]);
        let out = stripped_of(&input);
        let image_len = fx::chunk_payload(&base, b"VP8L").unwrap().len();
        assert_eq!(
            out.len(),
            12 + (8 + 10) + (8 + ICC_EVEN.len()) + (8 + image_len + image_len % 2)
        );
        assert_eq!(out, input);
    }

    #[test]
    fn writes_the_missing_pad_of_a_final_odd_chunk() {
        // The last chunk is odd-sized and the file ends without its pad byte,
        // and the RIFF size leaves it out, as some writers do.
        let chunks = [
            fx::chunks_of(&lossless_file(false)).remove(0),
            (*b"ICCP", vec![1, 2, 3]),
        ];
        let with_pad = fx::from_chunks(&chunks);
        let mut without_pad = with_pad.clone();
        without_pad.pop();
        let size = u32::from_le_bytes(without_pad[4..8].try_into().unwrap()) - 1;
        without_pad[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(parse(&without_pad).is_ok(), "the fixture is accepted");

        let out = stripped_of(&without_pad);

        assert_eq!(out.len(), without_pad.len() + 1);
        assert_eq!(out.last(), Some(&0));
        assert_well_formed(&out);
        // The same file with its pad is copied byte for byte.
        assert_eq!(stripped_of(&with_pad), with_pad);
    }

    #[test]
    fn drops_bytes_after_the_riff_size() {
        let base = lossless_file(false);
        let extended = fx::with_chunks(&base, &[(*b"ICCP", ICC_EVEN)]);
        for clean in [&base, &extended] {
            let tail = [0xAB; 100];
            let mut input = clean.clone();
            input.extend_from_slice(&tail);

            let out = stripped_of(&input);

            assert!(!out.windows(tail.len()).any(|w| w == tail), "tail kept");
            assert_eq!(&out, clean);
            assert_well_formed(&out);
            assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(clean));
        }
        // A whole chunk after the RIFF size does not come in either.
        let mut with_chunk = base.clone();
        with_chunk.extend(fx::from_chunks(&[(*b"EXIF", vec![5; 40])])[12..].to_vec());
        assert_eq!(stripped_of(&with_chunk), base);
    }

    #[test]
    fn rewrites_flags_that_disagree_with_the_chunks() {
        let image = || fx::chunks_of(&lossless_file(false)).remove(0);
        let vp8x = |flags: u8| (*b"VP8X", fx::vp8x_payload(flags, SIZE, SIZE));
        let rotated = || (*b"EXIF", fx::exif_with_orientation(6, false, true));
        // (the chunks, the flags the output must have)
        let cases: Vec<(Vec<fx::RawChunk>, u8)> = vec![
            // An EXIF chunk with its flag clear: dropped, flag stays clear.
            (vec![vp8x(0), image(), (*b"EXIF", vec![1; 8])], 0),
            // The XMP flag set with no XMP chunk: cleared.
            (vec![vp8x(fx::FLAG_XMP), image()], 0),
            // The EXIF flag set with no EXIF chunk: cleared.
            (vec![vp8x(fx::FLAG_EXIF), image()], 0),
            // The ICC flag set with no ICCP chunk: cleared, like the others.
            (vec![vp8x(fx::FLAG_ICC), image()], 0),
            // A rotated EXIF chunk with its flag clear: kept, the flag set.
            (vec![vp8x(0), image(), rotated()], fx::FLAG_EXIF),
            // A rotated EXIF chunk with its flag set, and an XMP flag with no
            // chunk: EXIF stays set, XMP is cleared.
            (
                vec![vp8x(fx::FLAG_EXIF | fx::FLAG_XMP), image(), rotated()],
                fx::FLAG_EXIF,
            ),
            // Several at once, with alpha and animation flags that stay.
            (
                vec![
                    vp8x(fx::FLAG_EXIF | fx::FLAG_XMP | fx::FLAG_ALPHA | fx::FLAG_ANIMATION),
                    image(),
                    (*b"XMP ", vec![1; 8]),
                ],
                fx::FLAG_ALPHA | fx::FLAG_ANIMATION,
            ),
        ];
        for (chunks, expected) in cases {
            let input = fx::from_chunks(&chunks);
            let out = stripped_of(&input);
            assert_eq!(flags(&out), expected, "{:?}", fx::chunk_names(&input));
            assert_well_formed(&out);
            // The EXIF and XMP flags agree with the chunks kept, and no ICC
            // flag is left without an ICCP chunk.
            let names = fx::chunk_names(&out);
            let has = |name: &str| names.iter().any(|n| n == name);
            assert_eq!(flags(&out) & fx::FLAG_EXIF != 0, has("EXIF"));
            assert!(!has("XMP "));
            assert_eq!(flags(&out) & fx::FLAG_XMP, 0);
            if !has("ICCP") {
                assert_eq!(flags(&out) & fx::FLAG_ICC, 0);
            }
        }
    }

    #[test]
    fn keeps_the_sources_icc_flag_when_the_file_has_an_iccp_chunk() {
        // The ruling "Keep the source's flag": with an ICCP chunk, the ICC
        // bit is copied, set or clear, whatever else is stripped.
        let image = || fx::chunks_of(&lossless_file(false)).remove(0);
        let iccp = || (*b"ICCP", ICC_EVEN.to_vec());
        for extra in [
            None,
            Some((*b"EXIF", vec![1; 8])),
            Some((*b"XMP ", vec![2; 8])),
        ] {
            for source_flags in [0, fx::FLAG_ICC, fx::FLAG_ICC | fx::FLAG_XMP, fx::FLAG_XMP] {
                let mut chunks = vec![
                    (*b"VP8X", fx::vp8x_payload(source_flags, SIZE, SIZE)),
                    iccp(),
                    image(),
                ];
                chunks.extend(extra.clone());
                let input = fx::from_chunks(&chunks);

                let out = stripped_of(&input);

                assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8L"]);
                assert_eq!(
                    flags(&out),
                    source_flags & fx::FLAG_ICC,
                    "source flags {source_flags:#04x}, extra {:?}",
                    extra.as_ref().map(|c| c.0)
                );
                assert_well_formed(&out);
            }
        }
    }

    // --- stripped: EXIF that carries a rotation ------------------------------------

    #[test]
    fn keeps_a_rotated_exif_chunk_byte_identical_and_sets_its_flag() {
        for (orientation, big_endian, prefix) in [
            (6, false, true),
            (8, true, false),
            (3, false, false),
            (6, true, true),
        ] {
            let exif = fx::exif_with_orientation(orientation, big_endian, prefix);
            let input = fx::with_chunks(
                &lossless_file(true),
                &[(*b"EXIF", &exif), (*b"XMP ", b"<x:xmpmeta/>")],
            );

            let out = stripped_of(&input);

            assert_well_formed(&out);
            assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L", "EXIF"]);
            assert_eq!(fx::chunk_payload(&out, b"EXIF"), Some(exif));
            assert_eq!(
                flags(&out),
                (flags(&input) & fx::FLAG_ALPHA) | fx::FLAG_EXIF,
                "orientation {orientation}"
            );
            assert!(out.len() < input.len(), "the XMP is gone");
            assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&input));
        }
    }

    #[test]
    fn drops_an_exif_chunk_that_carries_no_rotation() {
        let unrotated: [Vec<u8>; 4] = [
            fx::exif_with_orientation(1, false, true),
            fx::exif_with_orientation(1, true, false),
            // No Orientation tag at all.
            fx::tiff(false, &[(0x0100, fx::SHORT, 1, 6)]),
            // Unreadable: the TIFF header cut short.
            b"Exif\0\0II*".to_vec(),
        ];
        for exif in unrotated {
            let input = fx::with_chunks(&lossless_file(false), &[(*b"EXIF", &exif)]);
            let out = stripped_of(&input);
            assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L"], "{exif:?}");
            assert_eq!(flags(&out) & fx::FLAG_EXIF, 0);
            assert_well_formed(&out);
        }
    }

    #[test]
    fn decides_on_the_first_exif_chunk_and_drops_any_other() {
        let image = || fx::chunks_of(&lossless_file(false)).remove(0);
        let exif = |orientation| {
            (
                *b"EXIF",
                fx::exif_with_orientation(orientation, false, true),
            )
        };
        let vp8x = (*b"VP8X", fx::vp8x_payload(fx::FLAG_EXIF, SIZE, SIZE));

        // Two rotated: only the first stays.
        let two_rotated = fx::from_chunks(&[vp8x.clone(), image(), exif(6), exif(8)]);
        let out = stripped_of(&two_rotated);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L", "EXIF"]);
        assert_eq!(fx::chunk_payload(&out, b"EXIF"), Some(exif(6).1));
        assert_eq!(flags(&out), fx::FLAG_EXIF);

        // The first does not rotate: none stays, though the second would.
        let first_unrotated = fx::from_chunks(&[vp8x, image(), exif(1), exif(6)]);
        let out = stripped_of(&first_unrotated);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L"]);
        assert_eq!(flags(&out), 0);
    }

    // --- stripped: a simple file (no VP8X) with extra chunks -------------------------

    #[test]
    fn keeps_an_iccp_chunk_of_a_simple_file_and_adds_no_vp8x() {
        let image = fx::chunks_of(&lossless_file(false)).remove(0);
        let input = fx::from_chunks(&[image.clone(), (*b"ICCP", ICC_ODD.to_vec())]);
        let out = stripped_of(&input);
        assert_eq!(out, input);
        assert_eq!(fx::chunk_names(&out), ["VP8L", "ICCP"]);

        // What is not kept still goes, and still no VP8X comes in.
        let with_xmp =
            fx::from_chunks(&[image, (*b"ICCP", ICC_ODD.to_vec()), (*b"XMP ", vec![3; 40])]);
        let out = stripped_of(&with_xmp);
        assert_eq!(out, input);
        assert_eq!(fx::chunk_names(&out), ["VP8L", "ICCP"]);
    }

    #[test]
    fn keeps_a_rotated_exif_chunk_of_a_simple_file_and_adds_no_vp8x() {
        let image = || fx::chunks_of(&lossless_file(false)).remove(0);
        let exif = |orientation| {
            (
                *b"EXIF",
                fx::exif_with_orientation(orientation, true, false),
            )
        };
        let rotated = fx::from_chunks(&[image(), exif(8)]);
        assert_eq!(stripped_of(&rotated), rotated);

        let unrotated = fx::from_chunks(&[image(), exif(1)]);
        assert_eq!(fx::chunk_names(&stripped_of(&unrotated)), ["VP8L"]);
    }

    #[test]
    fn keeps_the_reserved_bits_the_canvas_and_a_longer_vp8x_header_as_they_were() {
        let mut payload = fx::vp8x_payload(0xC1 | fx::FLAG_ALPHA | fx::FLAG_EXIF, SIZE, SIZE);
        payload[1..4].copy_from_slice(&[7, 8, 9]);
        payload.extend_from_slice(&[0xEE; 4]);
        let input = fx::from_chunks(&[
            (*b"VP8X", payload.clone()),
            fx::chunks_of(&lossless_file(true)).remove(0),
        ]);

        let out = stripped_of(&input);

        let mut expected = payload;
        expected[0] = 0xC1 | fx::FLAG_ALPHA;
        assert_eq!(fx::chunk_payload(&out, b"VP8X"), Some(expected));
    }

    #[test]
    fn writes_a_riff_size_that_covers_exactly_the_file() {
        let base = lossless_file(true);
        let inputs = [
            base.clone(),
            fx::with_chunks(&base, &[(*b"ICCP", ICC_ODD), (*b"EXIF", &[0; 301])]),
            fx::animated(std::slice::from_ref(&base), SIZE, SIZE),
        ];
        for input in inputs {
            let out = stripped_of(&input);
            let declared = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
            assert_eq!(declared + 8, out.len());
            assert_eq!(&out[8..12], b"WEBP");
        }
    }

    // --- with_metadata: a lossy result with the source's metadata --------------------

    /// A lossy result as libwebp writes it: a simple `VP8 ` file, or with
    /// `alpha` a `VP8X`, `ALPH`, `VP8 ` file.
    fn encoded(alpha: bool) -> Vec<u8> {
        fx::lossy(&pixels(alpha), SIZE, SIZE, 80.0)
    }

    fn rotated_exif() -> Vec<u8> {
        fx::exif_with_orientation(6, false, true)
    }

    fn with_metadata_of(encoded: &[u8], source: &[u8]) -> Result<Vec<u8>, String> {
        with_metadata(encoded, &parse(source).unwrap())
    }

    /// The canvas a `VP8X` payload declares.
    fn canvas(file: &[u8]) -> (u32, u32) {
        let payload = fx::chunk_payload(file, b"VP8X").unwrap();
        let read =
            |at: usize| u32::from_le_bytes([payload[at], payload[at + 1], payload[at + 2], 0]);
        (read(4) + 1, read(7) + 1)
    }

    #[test]
    fn with_metadata_adds_iccp_and_a_rotated_exif_to_a_simple_file() {
        let result = encoded(false);
        assert_eq!(fx::chunk_names(&result), ["VP8 "], "fixture");
        let source = fx::with_chunks(
            &lossless_file(false),
            &[(*b"ICCP", ICC_EVEN), (*b"EXIF", &rotated_exif())],
        );

        let out = with_metadata_of(&result, &source).unwrap();

        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8 ", "EXIF"]);
        assert_eq!(fx::chunk_payload(&out, b"ICCP").as_deref(), Some(ICC_EVEN));
        assert_eq!(fx::chunk_payload(&out, b"EXIF"), Some(rotated_exif()));
        assert_eq!(
            fx::chunk_payload(&out, b"VP8 "),
            fx::chunk_payload(&result, b"VP8 ")
        );
        assert_eq!(flags(&out), fx::FLAG_ICC | fx::FLAG_EXIF);
        assert_eq!(canvas(&out), (SIZE, SIZE));
        assert_eq!(&fx::chunk_payload(&out, b"VP8X").unwrap()[1..4], [0, 0, 0]);
        assert_well_formed(&out);
    }

    #[test]
    fn with_metadata_keeps_alph_before_the_image_and_sets_the_alpha_flag() {
        let result = encoded(true);
        assert_eq!(
            fx::chunk_names(&result),
            ["VP8X", "ALPH", "VP8 "],
            "fixture"
        );
        let source = fx::with_chunks(&lossless_file(true), &[(*b"ICCP", ICC_ODD)]);

        let out = with_metadata_of(&result, &source).unwrap();

        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "ALPH", "VP8 "]);
        assert_eq!(
            fx::chunk_payload(&out, b"ALPH"),
            fx::chunk_payload(&result, b"ALPH")
        );
        assert_eq!(flags(&out), fx::FLAG_ALPHA | fx::FLAG_ICC);
        assert_well_formed(&out);

        // With nothing to add, libwebp's own alpha file comes back as written.
        let plain = with_metadata_of(&result, &lossless_file(true)).unwrap();
        assert_eq!(plain, result);
    }

    #[test]
    fn with_metadata_writes_no_vp8x_when_nothing_needs_one() {
        let result = encoded(false);
        // No metadata at all, and metadata the lossless path drops too.
        let sources = [
            lossless_file(false),
            fx::with_chunks(
                &lossless_file(false),
                &[
                    (*b"XMP ", b"<x:xmpmeta/>"),
                    (*b"EXIF", &fx::exif_with_orientation(1, true, false)),
                    (*b"ABCD", &[5; 9]),
                ],
            ),
        ];
        for source in sources {
            let out = with_metadata_of(&result, &source).unwrap();
            assert_eq!(out, result);
            assert_eq!(fx::chunk_names(&out), ["VP8 "]);
        }
    }

    #[test]
    fn with_metadata_pads_odd_chunks_and_writes_the_riff_size() {
        let result = encoded(false);
        let image_len = fx::chunk_payload(&result, b"VP8 ").unwrap().len();
        let mut odd_exif = rotated_exif();
        odd_exif.push(0x99);
        assert_eq!(odd_exif.len() % 2, 1, "fixture");
        let source = fx::with_chunks(
            &lossless_file(false),
            &[(*b"ICCP", ICC_ODD), (*b"EXIF", &odd_exif)],
        );

        let out = with_metadata_of(&result, &source).unwrap();

        assert_eq!(
            out.len(),
            12 + (8 + 10)
                + (8 + ICC_ODD.len() + 1)
                + (8 + image_len + image_len % 2)
                + (8 + odd_exif.len() + 1)
        );
        assert_eq!(fx::chunk_payload(&out, b"ICCP").as_deref(), Some(ICC_ODD));
        assert_eq!(fx::chunk_payload(&out, b"EXIF"), Some(odd_exif));
        assert_well_formed(&out);
    }

    #[test]
    fn with_metadata_refuses_bytes_that_are_not_webp() {
        let source = lossless_file(false);
        let whole = encoded(false);
        for bytes in [&b"not a WebP"[..], b"", &whole[..whole.len() - 6]] {
            let err = with_metadata_of(bytes, &source).unwrap_err();
            assert!(
                err.starts_with("the lossy result is not a readable WebP file ("),
                "{err}"
            );
        }
        assert!(with_metadata_of(&whole, &source).is_ok());
    }

    #[test]
    fn with_metadata_refuses_an_empty_image() {
        let source = lossless_file(false);
        assert_eq!(
            with_metadata_of(&riff(&[vp8(0, 16)]), &source).unwrap_err(),
            "the lossy result has an empty image (0×16)"
        );
        assert_eq!(
            with_metadata_of(&riff(&[vp8(16, 0)]), &source).unwrap_err(),
            "the lossy result has an empty image (16×0)"
        );
        // The same headers with a size are taken.
        assert!(with_metadata_of(&riff(&[vp8(16, 16)]), &source).is_ok());
    }

    // --- with_metadata: an animated result ----------------------------------------------

    /// An animation as a lossy encoder could write it: two frames of
    /// `size`x`size`, timed 120 and 80 ms, looping twice, on a background.
    fn animated_result(size: u32) -> Vec<u8> {
        let frames = [
            fx::lossy(&fx::photo_rgba(size, size, true), size, size, 70.0),
            fx::lossy(&fx::photo_rgba(size, size, false), size, size, 70.0),
        ];
        fx::animated_with(&frames, size, size, &[120, 80], 2, 0x8040_2010)
    }

    /// An animated source with a profile, a rotation and XMP.
    fn animated_source() -> Vec<u8> {
        let frames = [lossless_file(false), lossless_file(true)];
        let animation = fx::animated(&frames, SIZE, SIZE);
        fx::with_chunks(
            &animation,
            &[
                (*b"ICCP", ICC_ODD),
                (*b"EXIF", &rotated_exif()),
                (*b"XMP ", b"<x:xmpmeta/>"),
            ],
        )
    }

    fn payloads_of(file: &[u8], fourcc: &[u8; 4]) -> Vec<Vec<u8>> {
        fx::chunks_of(file)
            .into_iter()
            .filter(|(name, _)| name == fourcc)
            .map(|(_, data)| data)
            .collect()
    }

    #[test]
    fn with_metadata_keeps_an_animation_and_adds_the_sources_metadata() {
        // The result's canvas differs from the source's: the canvas written
        // is the result's own.
        let result = animated_result(24);
        let source = animated_source();

        let out = with_metadata_of(&result, &source).unwrap();

        assert_eq!(
            fx::chunk_names(&out),
            ["VP8X", "ICCP", "ANIM", "ANMF", "ANMF", "EXIF"]
        );
        assert_eq!(payloads_of(&out, b"ANIM"), payloads_of(&result, b"ANIM"));
        assert_eq!(payloads_of(&out, b"ANMF"), payloads_of(&result, b"ANMF"));
        assert_eq!(payloads_of(&out, b"ICCP"), [ICC_ODD]);
        assert_eq!(payloads_of(&out, b"EXIF"), [rotated_exif()]);
        assert_eq!(
            flags(&out),
            fx::FLAG_ANIMATION | fx::FLAG_ALPHA | fx::FLAG_ICC | fx::FLAG_EXIF
        );
        assert_eq!(canvas(&out), (24, 24));
        assert_well_formed(&out);
        // An independent decoder sees the result's frames and timings.
        assert_eq!(fx::decode_frames(&out), fx::decode_frames(&result));
    }

    #[test]
    fn with_metadata_keeps_a_clear_icc_flag_on_an_animation() {
        let mut source = animated_source();
        source[fx::VP8X_FLAGS_AT] &= !fx::FLAG_ICC;
        let out = with_metadata_of(&animated_result(24), &source).unwrap();
        assert_eq!(payloads_of(&out, b"ICCP"), [ICC_ODD]);
        assert_eq!(flags(&out) & fx::FLAG_ICC, 0);
        assert_eq!(flags(&out) & fx::FLAG_ANIMATION, fx::FLAG_ANIMATION);
    }

    #[test]
    fn with_metadata_gives_a_still_result_no_animation_flag() {
        // An animated source, a still result: the result decides.
        let out = with_metadata_of(&encoded(false), &animated_source()).unwrap();
        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8 ", "EXIF"]);
        assert_eq!(flags(&out), fx::FLAG_ICC | fx::FLAG_EXIF);
    }

    #[test]
    fn with_metadata_refuses_an_animation_without_vp8x() {
        let result = riff(&[anim(), anmf_header(SIZE, SIZE, 100)]);
        assert_eq!(parse(&result).unwrap().kind, Kind::Animated, "fixture");
        assert_eq!(
            with_metadata_of(&result, &lossless_file(false)).unwrap_err(),
            "the lossy result is an animation without a VP8X chunk"
        );
    }

    // --- animation_header ------------------------------------------------------------

    #[test]
    fn animation_header_reads_the_canvas_the_frames_and_the_total_duration() {
        let bytes = riff(&[
            vp8x(0x02, 300, 200),
            anim(),
            anmf_header(300, 200, 100),
            anmf_header(10, 10, 250),
            anmf_header(300, 200, 70),
        ]);
        let file = parse_quickly(&bytes).unwrap();
        assert_eq!(
            animation_header(&file),
            Ok(AnimationHeader {
                width: 300,
                height: 200,
                frames: 3,
                duration_ms: 420,
            })
        );
        // The largest 24-bit duration, twice: the flags byte after it is not
        // part of the number.
        let bytes = riff(&[
            vp8x(0x02, 1, 1),
            anim(),
            anmf_header(1, 1, 0xFF_FFFF),
            anmf_header(1, 1, 0xFF_FFFF),
        ]);
        let header = animation_header(&parse_quickly(&bytes).unwrap()).unwrap();
        assert_eq!((header.frames, header.duration_ms), (2, 2 * 0xFF_FFFF));
        // A real animation from the fixtures.
        let frames = [lossless_file(true), lossless_file(false)];
        let real = fx::animated_with(&frames, SIZE, SIZE, &[40, 960], 0, 0);
        let header = animation_header(&parse_quickly(&real).unwrap()).unwrap();
        assert_eq!(
            header,
            AnimationHeader {
                width: SIZE,
                height: SIZE,
                frames: 2,
                duration_ms: 1000,
            }
        );
    }

    #[test]
    fn animation_header_refuses_an_animation_without_vp8x_or_with_a_cut_anmf_header() {
        let no_vp8x = riff(&[anim(), anmf_header(8, 8, 100)]);
        let err = animation_header(&parse_quickly(&no_vp8x).unwrap()).unwrap_err();
        assert_eq!(
            err,
            "could not read the WebP data: the animation has no VP8X chunk"
        );
        let mut cut = anmf_header(8, 8, 100);
        // 15 bytes of header: the chunk size, the payload and its pad.
        cut[4] = 15;
        cut.truncate(8 + 15);
        cut.push(0);
        let bytes = riff(&[vp8x(0x02, 8, 8), anim(), anmf_header(8, 8, 100), cut]);
        let err = animation_header(&parse_quickly(&bytes).unwrap()).unwrap_err();
        assert_eq!(
            err,
            "could not read the WebP data: an ANMF chunk is shorter than its 16-byte header"
        );
    }

    #[test]
    fn with_metadata_sets_the_alpha_flag_for_a_vp8l_with_alpha() {
        let source = fx::with_chunks(&lossless_file(false), &[(*b"ICCP", ICC_EVEN)]);
        let with_alpha = lossless_file(true);
        // `image`'s encoder sets the alpha bit on every file; clear it for an
        // image that says it is opaque (the bit is only a hint to readers).
        let mut opaque_chunks = fx::chunks_of(&lossless_file(false));
        opaque_chunks[0].1[4] &= !0x10;
        let opaque = fx::from_chunks(&opaque_chunks);
        let alpha_bit = |file: &[u8]| {
            let payload = fx::chunk_payload(file, b"VP8L").unwrap();
            u32::from_le_bytes(payload[1..5].try_into().unwrap()) >> 28 & 1
        };
        assert_eq!(
            (alpha_bit(&with_alpha), alpha_bit(&opaque)),
            (1, 0),
            "fixture"
        );

        let out = with_metadata_of(&with_alpha, &source).unwrap();
        assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8L"]);
        assert_eq!(flags(&out), fx::FLAG_ALPHA | fx::FLAG_ICC);

        let out = with_metadata_of(&opaque, &source).unwrap();
        assert_eq!(flags(&out), fx::FLAG_ICC);
    }
}
