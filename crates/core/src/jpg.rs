//! JPG optimisation: a lossless coefficient transcode with mozjpeg, the
//! jpegtran route chosen in the Step 2 evaluation.
//!
//! The quantised DCT coefficients are read from the source and written back
//! unchanged; only the entropy coding (Huffman tables and scan layout) is
//! rebuilt. Nothing is re-quantised, so every decoder sees the same image.
//!
//! This is the only module with `unsafe` code; the crate denies it elsewhere.
//! libjpeg reports a fatal error by calling `error_exit`, which must not
//! return. Ours unwinds with a [`LibjpegFailure`] payload through the C frames
//! (`extern "C-unwind"`; mozjpeg-sys's `unwinding` feature compiles libjpeg
//! with `-fexceptions`), and [`run_libjpeg`] catches it. Every libjpeg object
//! is owned by a guard whose `Drop` destroys it, so the objects are also freed
//! when a failure unwinds past them. The unwinding route needs
//! `panic = "unwind"`, the default: never set `panic = "abort"`.

use std::mem;
use std::os::raw::{c_int, c_long, c_ulong};
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::slice;

use mozjpeg_sys::{
    JCOPY_OPTION, JCOPY_OPTION_JCOPYOPT_ALL, JCP_FASTEST, JCP_MAX_COMPRESSION,
    JINT_COMPRESS_PROFILE, boolean, jcopy_markers_execute, jcopy_markers_setup,
    jpeg_c_set_int_param, jpeg_common_struct, jpeg_compress_struct, jpeg_copy_critical_parameters,
    jpeg_create_compress, jpeg_create_decompress, jpeg_decompress_struct, jpeg_destination_mgr,
    jpeg_destroy_compress, jpeg_destroy_decompress, jpeg_error_mgr, jpeg_finish_compress,
    jpeg_finish_decompress, jpeg_mem_src, jpeg_read_coefficients, jpeg_read_header,
    jpeg_simple_progression, jpeg_std_error, jpeg_write_coefficients, jvirt_barray_control,
};

/// Every JPEG starts with the SOI marker, followed by the next marker's 0xFF.
const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// Markers copied from the source: every APPn and COM segment (EXIF, ICC,
/// XMP, comments), as jpegoptim keeps by default.
const COPY_MARKERS: JCOPY_OPTION = JCOPY_OPTION_JCOPYOPT_ALL;

/// libjpeg's `JMSG_LENGTH_MAX` (jpeglib.h): `format_message` writes up to
/// this many bytes, NUL included (jerror.c passes it to `snprintf`). The
/// binding's `&[u8; 80]` parameter type is wrong: 80 is `JMSG_STR_PARM_MAX`,
/// the size of a message's string parameter, not of the message.
const JMSG_LENGTH_MAX: usize = 200;

/// Most memory libjpeg may use for its image arrays (1 GiB, which fits the
/// 32-bit `c_long` of Windows). The coefficients take 2 bytes per sample. A
/// 100-megapixel CMYK image without subsampling (four components, the most a
/// real-world JPEG has) needs 800 MB, within the cap. libjpeg allows up to 10
/// components (`MAX_COMPONENTS`), and such a file at 100 megapixels would need
/// 2 GB: the cap refuses it through `error_exit` instead of overcommitting.
const MAX_LIBJPEG_MEMORY: c_long = 1 << 30;

/// First size of the output buffer. It doubles whenever libjpeg fills it.
const INITIAL_OUTPUT_BYTES: usize = 64 * 1024;

/// Optimise JPEG bytes losslessly and return the smallest result.
///
/// Two candidates are written from one decode of the source, both with every
/// marker copied and a JFIF APP0 only when the source had one:
/// - A, jpegoptim's default: libjpeg's settings (`JCP_FASTEST`) with
///   optimised Huffman tables, in the source's own mode (progressive only when
///   the source is);
/// - B, `mozjpeg jpegtran`'s default: `JCP_MAX_COMPRESSION`, progressive with
///   the scan layout searched for the smallest file.
///
/// The smaller one is returned; when neither is smaller than the source, the
/// source bytes themselves are, so an already-optimal file is copied as it is.
///
/// The error is a message for the user: the content is not a JPEG, or libjpeg
/// could not read it. A warning from the decoder (for example a truncated
/// scan, which libjpeg would pad and carry on with) is an error too, so a
/// damaged file is never written out as if it were whole.
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&SOI) {
        return Err("not a JPEG file (the content is not JPEG data)".to_owned());
    }
    let candidates = candidates(bytes)?;
    Ok(smallest(bytes, &candidates).to_vec())
}

/// The first of the smallest candidates, if it is smaller than `original`;
/// otherwise `original`. On a tie between candidates the first one wins
/// (candidate A, jpegoptim's result); a candidate of the same size as the
/// original does not count as smaller.
fn smallest<'a>(original: &'a [u8], candidates: &'a [Vec<u8>]) -> &'a [u8] {
    candidates
        .iter()
        .map(Vec::as_slice)
        .fold(original, |best, candidate| {
            if candidate.len() < best.len() {
                candidate
            } else {
                best
            }
        })
}

/// Candidates A and B (see [`optimize`]), in that order.
fn candidates(bytes: &[u8]) -> Result<[Vec<u8>; 2], String> {
    // `c_ulong` is 32 bits on Windows; the planner's size limit is far below.
    let len = c_ulong::try_from(bytes.len()).map_err(|_| "the file is too large".to_owned())?;
    // SAFETY: `transcode` must run inside `run_libjpeg`, which it does here,
    // and `len` is `bytes.len()`.
    run_libjpeg(|| unsafe { transcode(bytes, len) })
}

/// How a candidate is entropy-coded.
#[derive(Clone, Copy)]
enum Coding {
    /// Candidate A: `JCP_FASTEST`, optimised Huffman tables, and the
    /// standard progressive script only when the source is progressive.
    LikeJpegoptim { progressive: bool },
    /// Candidate B: mozjpeg's default profile, `JCP_MAX_COMPRESSION`.
    MaxCompression,
}

/// Decode the coefficients of `bytes` once and write both candidates.
///
/// # Safety
///
/// Must run inside [`run_libjpeg`]: libjpeg failures unwind out of it. `len`
/// must be `bytes.len()`.
unsafe fn transcode(bytes: &[u8], len: c_ulong) -> [Vec<u8>; 2] {
    let mut decompress = Decompress::new();
    let source = decompress.cinfo();
    // SAFETY: the standard libjpeg transcode sequence on a decompressor that
    // `Decompress` created and will destroy. `bytes` outlives `decompress`
    // (dropped at the end of this function), and libjpeg only reads through
    // that pointer, at most `len` bytes. The coefficient arrays and the saved
    // markers live in the decompressor's memory pool until
    // `jpeg_finish_decompress`, which runs after both candidates are written.
    unsafe {
        jpeg_mem_src(source, bytes.as_ptr(), len);
        jcopy_markers_setup(source, COPY_MARKERS);
        // With `require_image` set, a file without an image fails inside; a
        // memory source never suspends, so the header is read when it returns.
        jpeg_read_header(source, 1);
        // Refuse a decompression bomb before `jpeg_read_coefficients`
        // allocates arrays for every declared block. Failing here unwinds
        // through `decompress`, whose guard destroys the decompressor.
        if let Err(message) = crate::plan::check_pixels(source.image_width, source.image_height) {
            fail(message);
        }
        let coefficients = jpeg_read_coefficients(source);
        if coefficients.is_null() {
            fail("could not read the JPEG data: libjpeg returned no coefficients".to_owned());
        }
        // `coef_bits` is documented as NULL for a non-progressive file
        // (`progressive_mode` itself is private in mozjpeg-sys).
        let progressive = !source.coef_bits.is_null();
        let jfif = has_jfif(source);
        let a = write_candidate(
            source,
            coefficients,
            Coding::LikeJpegoptim { progressive },
            jfif,
        );
        let b = write_candidate(source, coefficients, Coding::MaxCompression, jfif);
        jpeg_finish_decompress(source);
        [a, b]
    }
}

/// Write one candidate from the source's coefficients.
///
/// # Safety
///
/// Must run inside [`run_libjpeg`]. `coefficients` must be what
/// `jpeg_read_coefficients` returned for `source`, which must not have been
/// finished yet. Writing only reads the coefficient arrays, so both candidates
/// can be written from one decode.
unsafe fn write_candidate(
    source: &mut jpeg_decompress_struct,
    coefficients: *mut *mut jvirt_barray_control,
    coding: Coding,
    jfif: bool,
) -> Vec<u8> {
    let mut compress = Compress::new();
    let output = compress.cinfo();
    // SAFETY: the standard libjpeg transcode sequence on a compressor that
    // `Compress` created, whose destination is its own `VecDestination`, and
    // which it destroys. `source` and `coefficients` are valid for the whole
    // call (this function's contract).
    unsafe {
        // Before `jpeg_copy_critical_parameters`: it calls
        // `jpeg_set_defaults`, which applies the profile; set afterwards, it
        // would come too late. B's profile is mozjpeg's default today, but it
        // is set explicitly so a version bump cannot change it silently.
        let profile = match coding {
            Coding::LikeJpegoptim { .. } => JCP_FASTEST,
            Coding::MaxCompression => JCP_MAX_COMPRESSION,
        };
        jpeg_c_set_int_param(output, JINT_COMPRESS_PROFILE, profile as c_int);
        jpeg_copy_critical_parameters(source, output);
        // libjpeg turns a JFIF APP0 on for every YCbCr or greyscale output;
        // jpegoptim and jpegtran write one only when the source had one. A
        // source JFIF that libjpeg would not write itself (another colour
        // space) is still copied verbatim with the other markers.
        output.write_JFIF_header = boolean::from(jfif && output.write_JFIF_header != 0);
        output.optimize_coding = 1;
        if let Coding::LikeJpegoptim { progressive: true } = coding {
            jpeg_simple_progression(output);
        }
        jpeg_write_coefficients(output, coefficients);
        jcopy_markers_execute(source, output, COPY_MARKERS);
        jpeg_finish_compress(output);
    }
    compress.into_output()
}

/// Whether the source has a JFIF APP0 segment. mozjpeg-sys keeps
/// `saw_JFIF_marker` private, so this tests the saved APP0 bytes, with the
/// same test transupp.c uses to drop a duplicate JFIF.
///
/// # Safety
///
/// `source.marker_list` must be libjpeg's list of saved markers: valid after
/// `jpeg_read_header` and until the decompressor is finished or destroyed.
unsafe fn has_jfif(source: &jpeg_decompress_struct) -> bool {
    let mut node = source.marker_list;
    while !node.is_null() {
        // SAFETY: a non-null node of libjpeg's list (this function's
        // contract), valid while the decompressor is.
        let marker = unsafe { &*node };
        if marker.marker == 0xE0 && marker.data_length >= 5 {
            // SAFETY: `data` holds `data_length` bytes, at least 5 (checked).
            let start = unsafe { slice::from_raw_parts(marker.data, 5) };
            if start == b"JFIF\0" {
                return true;
            }
        }
        node = marker.next;
    }
    false
}

// --- failures: libjpeg's errors and warnings become `Err` ---------------------

/// The payload of a libjpeg failure on its way to [`run_libjpeg`].
struct LibjpegFailure(String);

/// Abandon the libjpeg call in progress: unwind to [`run_libjpeg`], which
/// returns `Err(message)`. `resume_unwind` does not run the panic hook, so
/// nothing is printed.
fn fail(message: String) -> ! {
    panic::resume_unwind(Box::new(LibjpegFailure(message)))
}

/// Run `f`, turning a libjpeg failure inside it into `Err`. Any other panic is
/// a bug and keeps unwinding.
fn run_libjpeg<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    // `AssertUnwindSafe`: after a failure nothing `f` worked on is used again;
    // its libjpeg objects were created inside `f` and destroyed by their
    // guards while unwinding.
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => match payload.downcast::<LibjpegFailure>() {
            Ok(failure) => Err(failure.0),
            Err(other) => panic::resume_unwind(other),
        },
    }
}

/// An error manager with our hooks in place of libjpeg's, which would print
/// to stderr and call `exit()`.
fn error_manager() -> jpeg_error_mgr {
    // SAFETY: all-zero is a valid `jpeg_error_mgr` (null pointers, `None`
    // hooks, zero counts).
    let mut err: jpeg_error_mgr = unsafe { mem::zeroed() };
    // SAFETY: `jpeg_std_error` fills in every field, cannot fail, and keeps no
    // pointer to the struct, so it may be moved afterwards.
    unsafe { jpeg_std_error(&mut err) };
    err.error_exit = Some(error_exit);
    err.emit_message = Some(emit_message);
    err.output_message = Some(output_message);
    err
}

/// libjpeg's text for the error or warning it just raised.
///
/// # Safety
///
/// `cinfo.err` must point to an error manager set up by [`error_manager`].
unsafe fn libjpeg_message(cinfo: &mut jpeg_common_struct) -> String {
    let mut buffer = [0u8; JMSG_LENGTH_MAX];
    // SAFETY: `err` is valid (this function's contract).
    if let Some(format) = unsafe { (*cinfo.err).format_message } {
        // The binding types the buffer as `&[u8; 80]`, which is wrong twice:
        // libjpeg writes into it, and writing through a shared reference is
        // undefined behaviour (the compiler may assume the buffer is still all
        // zeros afterwards); and it writes up to `JMSG_LENGTH_MAX` (200)
        // bytes, not 80. Calling through a signature with `*mut u8` makes the
        // write legitimate and carries no size, so the buffer passed can be
        // the 200 bytes libjpeg needs. Both signatures have the same ABI (one
        // thin pointer).
        // SAFETY: as just described, an ABI-compatible function pointer type.
        let format = unsafe {
            mem::transmute::<
                unsafe extern "C-unwind" fn(&mut jpeg_common_struct, &[u8; 80]),
                unsafe extern "C-unwind" fn(&mut jpeg_common_struct, *mut u8),
            >(format)
        };
        // SAFETY: libjpeg writes a NUL-terminated string of at most
        // `JMSG_LENGTH_MAX` bytes, and `buffer` holds that many.
        unsafe { format(cinfo, buffer.as_mut_ptr()) };
    }
    let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..len]).into_owned()
}

/// libjpeg's fatal-error hook. It must not return, so it unwinds.
unsafe extern "C-unwind" fn error_exit(cinfo: &mut jpeg_common_struct) {
    // SAFETY: libjpeg calls this only on an object whose `err` is one of ours.
    let message = unsafe { libjpeg_message(cinfo) };
    fail(format!("could not read the JPEG data: {message}"))
}

/// libjpeg's hook for warnings (level -1) and trace messages (level 0 and
/// up). A warning means libjpeg found damaged data and is patching over it (a
/// truncated scan is padded, a bad Huffman code skipped); writing that out
/// would store the damage as if it were the image, so a warning fails the
/// file. libjpeg's jerror.c names aborting on warnings as a reason to replace
/// this hook. Trace messages are dropped: a library never prints.
unsafe extern "C-unwind" fn emit_message(cinfo: &mut jpeg_common_struct, level: c_int) {
    if level < 0 {
        // SAFETY: as in `error_exit`.
        let message = unsafe { libjpeg_message(cinfo) };
        fail(format!("the JPEG data is damaged ({message})"));
    }
}

/// libjpeg's printing hook. Nothing calls it once the two hooks above are in
/// place; it is replaced too, so that nothing can ever print to stderr.
unsafe extern "C-unwind" fn output_message(_cinfo: &mut jpeg_common_struct) {}

// --- owned libjpeg objects ------------------------------------------------------

/// A libjpeg decompressor and its error manager. They are on the heap so their
/// addresses never change (libjpeg keeps a pointer to the error manager), and
/// they are reached only through a raw pointer, so moving the guard does not
/// invalidate that pointer. Dropping the guard destroys the decompressor in
/// whatever state a failure left it.
struct Decompress {
    state: NonNull<DecompressState>,
}

struct DecompressState {
    cinfo: jpeg_decompress_struct,
    err: jpeg_error_mgr,
}

impl Decompress {
    /// Create a decompressor. Creation can fail (out of memory), so call it
    /// inside [`run_libjpeg`].
    fn new() -> Self {
        let state = Box::new(DecompressState {
            // SAFETY: all-zero is the state `jpeg_create_decompress` expects
            // (null pointers, `global_state` 0), and one that
            // `jpeg_destroy_decompress` accepts.
            cinfo: unsafe { mem::zeroed() },
            err: error_manager(),
        });
        // The guard exists before `jpeg_create_decompress`, so an object whose
        // creation failed is still destroyed.
        let guard = Decompress {
            state: NonNull::from(Box::leak(state)),
        };
        let state = guard.state.as_ptr();
        // SAFETY: `state` is valid and owned by `guard` alone. The error
        // manager is installed before creation, as libjpeg requires.
        unsafe {
            (*state).cinfo.common.err = &raw mut (*state).err;
            jpeg_create_decompress(&raw mut (*state).cinfo);
            // `mem` is set by a successful `jpeg_create_decompress`.
            (*(*state).cinfo.common.mem).max_memory_to_use = MAX_LIBJPEG_MEMORY;
        }
        guard
    }

    fn cinfo(&mut self) -> &mut jpeg_decompress_struct {
        // SAFETY: the state is valid until `drop`, and `&mut self` makes this
        // the only reference to it.
        unsafe { &mut (*self.state.as_ptr()).cinfo }
    }
}

impl Drop for Decompress {
    fn drop(&mut self) {
        let state = self.state.as_ptr();
        // SAFETY: `jpeg_destroy_decompress` frees every pool of the object in
        // any state after `jpeg_create_decompress` (including mid-decode, after
        // a failure unwound out of libjpeg) and does nothing to a zeroed one;
        // it raises no errors. The box was leaked in `new` and is freed here
        // exactly once.
        unsafe {
            jpeg_destroy_decompress(&mut (*state).cinfo);
            drop(Box::from_raw(state));
        }
    }
}

/// A libjpeg compressor, its error manager and its output buffer: like
/// [`Decompress`], on the heap, reached through a raw pointer, and destroyed
/// on drop.
struct Compress {
    state: NonNull<CompressState>,
}

struct CompressState {
    cinfo: jpeg_compress_struct,
    err: jpeg_error_mgr,
    dest: VecDestination,
}

/// The compressor's destination: libjpeg's manager, followed by the Rust
/// buffer it fills. `repr(C)` with `mgr` first, so libjpeg's pointer to `mgr`
/// is also a pointer to the whole struct. The buffer is a `Vec`, so it is
/// freed with the guard whatever happens; libjpeg's own `jpeg_mem_dest`
/// would leak its `malloc` buffer on a failure.
#[repr(C)]
struct VecDestination {
    mgr: jpeg_destination_mgr,
    buffer: Vec<u8>,
}

impl Compress {
    /// Create a compressor that writes into a `Vec`. Creation can fail (out
    /// of memory), so call it inside [`run_libjpeg`].
    fn new() -> Self {
        let state = Box::new(CompressState {
            // SAFETY: as in `Decompress::new`.
            cinfo: unsafe { mem::zeroed() },
            err: error_manager(),
            dest: VecDestination {
                mgr: jpeg_destination_mgr {
                    next_output_byte: ptr::null_mut(),
                    free_in_buffer: 0,
                    init_destination: Some(init_destination),
                    empty_output_buffer: Some(empty_output_buffer),
                    term_destination: Some(term_destination),
                },
                buffer: Vec::new(),
            },
        });
        let guard = Compress {
            state: NonNull::from(Box::leak(state)),
        };
        let state = guard.state.as_ptr();
        // SAFETY: as in `Decompress::new`. `jpeg_create_compress` zeroes the
        // struct apart from `err`, so `dest` is set after it; it points into
        // the same allocation and stays valid until `drop`.
        unsafe {
            (*state).cinfo.common.err = &raw mut (*state).err;
            jpeg_create_compress(&raw mut (*state).cinfo);
            (*(*state).cinfo.common.mem).max_memory_to_use = MAX_LIBJPEG_MEMORY;
            (*state).cinfo.dest = (&raw mut (*state).dest).cast::<jpeg_destination_mgr>();
        }
        guard
    }

    fn cinfo(&mut self) -> &mut jpeg_compress_struct {
        // SAFETY: as in `Decompress::cinfo`.
        unsafe { &mut (*self.state.as_ptr()).cinfo }
    }

    /// The bytes written, once `jpeg_finish_compress` has run.
    fn into_output(self) -> Vec<u8> {
        // SAFETY: the state is valid until `drop`, which runs after this, and
        // nothing else refers to it now.
        unsafe { mem::take(&mut (*self.state.as_ptr()).dest.buffer) }
    }
}

impl Drop for Compress {
    fn drop(&mut self) {
        let state = self.state.as_ptr();
        // SAFETY: as in `Decompress::drop`, with `jpeg_destroy_compress`.
        unsafe {
            jpeg_destroy_compress(&mut (*state).cinfo);
            drop(Box::from_raw(state));
        }
    }
}

/// The `VecDestination` a compressor writes into.
///
/// # Safety
///
/// `cinfo` must be a compressor made by [`Compress::new`], whose `dest` points
/// to its `VecDestination`. (mozjpeg points `dest` at a buffer of its own
/// while it tries scan layouts, but restores it before it calls our hooks.)
unsafe fn destination(cinfo: &mut jpeg_compress_struct) -> &mut VecDestination {
    // SAFETY: this function's contract; the destination is a separate field
    // of the state, so it does not overlap `cinfo`.
    unsafe { &mut *cinfo.dest.cast::<VecDestination>() }
}

/// Grow the buffer to twice the `used` bytes already in it (at least the
/// initial size), and point libjpeg at the free space after them.
fn offer_space(dest: &mut VecDestination, used: usize) {
    let len = used.saturating_mul(2).max(INITIAL_OUTPUT_BYTES);
    let extra = len - dest.buffer.len();
    if dest.buffer.try_reserve_exact(extra).is_err() {
        fail("not enough memory for the output".to_owned());
    }
    dest.buffer.resize(len, 0);
    dest.mgr.next_output_byte = dest.buffer[used..].as_mut_ptr();
    dest.mgr.free_in_buffer = len - used;
}

/// libjpeg's "start writing" hook.
unsafe extern "C-unwind" fn init_destination(cinfo: &mut jpeg_compress_struct) {
    // SAFETY: libjpeg calls this hook only through our own manager.
    let dest = unsafe { destination(cinfo) };
    dest.buffer.clear();
    offer_space(dest, 0);
}

/// libjpeg's "the buffer is full" hook: the whole buffer holds output.
unsafe extern "C-unwind" fn empty_output_buffer(cinfo: &mut jpeg_compress_struct) -> boolean {
    // SAFETY: as in `init_destination`.
    let dest = unsafe { destination(cinfo) };
    let used = dest.buffer.len();
    offer_space(dest, used);
    1
}

/// libjpeg's "done writing" hook: keep only the bytes written.
unsafe extern "C-unwind" fn term_destination(cinfo: &mut jpeg_compress_struct) {
    // SAFETY: as in `init_destination`.
    let dest = unsafe { destination(cinfo) };
    let used = dest.buffer.len().saturating_sub(dest.mgr.free_in_buffer);
    dest.buffer.truncate(used);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fixtures ---------------------------------------------------------

    /// RGB pixels: gradients plus deterministic noise.
    fn photo_pixels(w: u32, h: u32) -> Vec<u8> {
        let mut seed = 0x2545_F491_u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 24) as u8
        };
        let mut pixels = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push(((x * 255) / w) as u8 / 2 + noise());
                pixels.push(((y * 255) / h) as u8 / 2 + noise());
                pixels.push((((x + y) * 255) / (w + h)) as u8 / 2 + noise());
            }
        }
        pixels
    }

    fn encode(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode(pixels, w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    /// A baseline JPEG with the standard Huffman tables.
    fn baseline_jpeg() -> Vec<u8> {
        encode(&photo_pixels(256, 192), 256, 192)
    }

    /// The same picture, progressive: candidate B of the baseline fixture.
    fn progressive_jpeg() -> Vec<u8> {
        let [_, b] = candidates(&baseline_jpeg()).unwrap();
        assert_eq!(frame_marker(&b), 0xC2, "fixture must be progressive");
        b
    }

    /// The start-of-frame marker: 0xC0 baseline, 0xC2 progressive.
    fn frame_marker(jpeg: &[u8]) -> u8 {
        let mut at = 2;
        while at + 4 <= jpeg.len() {
            let marker = jpeg[at + 1];
            if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                return marker;
            }
            at += 2 + u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        }
        panic!("no SOF marker");
    }

    /// Every quantised DCT coefficient of every block, with the quantisation
    /// tables and the sampling layout. Two files with equal `Coefficients`
    /// decode to the same pixels in any conforming decoder.
    #[derive(Debug, PartialEq)]
    struct Coefficients {
        width: u32,
        height: u32,
        components: Vec<Component>,
    }

    #[derive(Debug, PartialEq)]
    struct Component {
        id: i32,
        sampling: (i32, i32),
        quant: Vec<u16>,
        blocks: (u32, u32),
        values: Vec<i16>,
    }

    /// Read the coefficients with `jpeg_read_coefficients` (test helper).
    fn coefficients(bytes: &[u8]) -> Coefficients {
        let len = c_ulong::try_from(bytes.len()).unwrap();
        run_libjpeg(|| {
            let mut decompress = Decompress::new();
            let source = decompress.cinfo();
            // SAFETY: as in `transcode`: `bytes` outlives `decompress`, and
            // `comp_info`, the quantisation tables and the coefficient arrays
            // live in its pool until it is dropped at the end of this closure.
            // `comp_info` has `num_components` entries, `quant_tbl_no` names a
            // table the frame uses, and each row and block index stays below
            // `height_in_blocks` and `width_in_blocks`.
            unsafe {
                jpeg_mem_src(source, bytes.as_ptr(), len);
                jpeg_read_header(source, 1);
                let arrays = jpeg_read_coefficients(source);
                assert!(!arrays.is_null());
                let access = (*source.common.mem).access_virt_barray.unwrap();
                let mut components = Vec::new();
                for index in 0..source.num_components as usize {
                    let info = &*source.comp_info.add(index);
                    let table = source.quant_tbl_ptrs[info.quant_tbl_no as usize];
                    let mut values = Vec::new();
                    for row in 0..info.height_in_blocks {
                        let rows = access(&mut source.common, *arrays.add(index), row, 1, 0);
                        let blocks = slice::from_raw_parts(*rows, info.width_in_blocks as usize);
                        for block in blocks {
                            values.extend_from_slice(block);
                        }
                    }
                    components.push(Component {
                        id: info.component_id,
                        sampling: (info.h_samp_factor, info.v_samp_factor),
                        quant: (*table).quantval.to_vec(),
                        blocks: (info.width_in_blocks, info.height_in_blocks),
                        values,
                    });
                }
                Coefficients {
                    width: source.image_width,
                    height: source.image_height,
                    components,
                }
            }
        })
        .unwrap()
    }

    // --- the smallest of the candidates, or the original ----------------------

    #[test]
    fn smallest_returns_the_smallest_candidate() {
        let original = vec![0u8; 10];
        let candidates = [vec![1u8; 8], vec![2u8; 6]];
        assert_eq!(smallest(&original, &candidates), &[2u8; 6]);
        let reversed = [vec![2u8; 6], vec![1u8; 8]];
        assert_eq!(smallest(&original, &reversed), &[2u8; 6]);
    }

    #[test]
    fn smallest_returns_the_original_when_no_candidate_is_smaller() {
        let original = vec![0u8; 10];
        // Equal size is not smaller: the original wins.
        let candidates = [vec![1u8; 10], vec![2u8; 12]];
        let chosen = smallest(&original, &candidates);
        assert_eq!(chosen.as_ptr(), original.as_ptr());
        assert_eq!(chosen, original.as_slice());
        assert_eq!(smallest(&original, &[]).as_ptr(), original.as_ptr());
    }

    #[test]
    fn smallest_keeps_the_first_candidate_when_two_tie() {
        let original = vec![0u8; 10];
        let candidates = [vec![1u8; 7], vec![2u8; 7]];
        assert_eq!(smallest(&original, &candidates), &[1u8; 7]);
    }

    // --- lossless: every DCT coefficient is kept ---------------------------------

    #[test]
    fn both_candidates_keep_every_dct_coefficient_of_a_baseline_source() {
        let source = baseline_jpeg();
        let expected = coefficients(&source);
        for candidate in candidates(&source).unwrap() {
            assert!(coefficients(&candidate) == expected, "coefficients differ");
        }
    }

    #[test]
    fn both_candidates_keep_every_dct_coefficient_of_a_progressive_source() {
        let source = progressive_jpeg();
        let expected = coefficients(&source);
        for candidate in candidates(&source).unwrap() {
            assert!(coefficients(&candidate) == expected, "coefficients differ");
        }
    }

    #[test]
    fn the_optimized_output_keeps_every_dct_coefficient() {
        let source = baseline_jpeg();
        let optimized = optimize(&source).unwrap();
        assert_ne!(optimized, source, "the fixture must be optimisable");
        assert!(coefficients(&optimized) == coefficients(&source));
    }

    #[test]
    fn the_coefficient_comparison_detects_a_changed_block() {
        // Same size and quality, so the same quantisation tables: only the
        // first 8x8 block of pixels differs.
        let mut pixels = photo_pixels(256, 192);
        for y in 0..8 {
            for x in 0..8 {
                pixels[(y * 256 + x) * 3] ^= 0x80;
            }
        }
        let original = coefficients(&baseline_jpeg());
        let other = coefficients(&encode(&pixels, 256, 192));
        assert_eq!(original.components[0].quant, other.components[0].quant);
        assert!(original != other);
    }

    // --- coding mode ------------------------------------------------------------

    #[test]
    fn candidate_a_keeps_a_baseline_source_baseline() {
        let [a, _] = candidates(&baseline_jpeg()).unwrap();
        assert_eq!(frame_marker(&a), 0xC0);
    }

    #[test]
    fn candidate_a_keeps_a_progressive_source_progressive() {
        let [a, _] = candidates(&progressive_jpeg()).unwrap();
        assert_eq!(frame_marker(&a), 0xC2);
    }

    #[test]
    fn candidate_b_is_progressive_for_a_baseline_source() {
        let [_, b] = candidates(&baseline_jpeg()).unwrap();
        assert_eq!(frame_marker(&b), 0xC2);
    }

    #[test]
    fn writing_both_candidates_from_one_decode_gives_the_bytes_of_separate_runs() {
        // Both candidates are written from one decode of the source; writing
        // one must not disturb the coefficients the other is written from.
        let source = baseline_jpeg();
        let first = candidates(&source).unwrap();
        let second = candidates(&source).unwrap();
        assert_eq!(first, second);
        assert_ne!(first[0], first[1]);
    }

    // --- errors -------------------------------------------------------------------

    #[test]
    fn rejects_bytes_without_a_jpeg_start_marker() {
        let err = optimize(b"\x89PNG\r\n\x1a\n").unwrap_err();
        assert!(err.contains("not a JPEG"), "{err}");
        assert!(optimize(&[0xFF, 0xD8]).is_err());
        assert!(optimize(b"").is_err());
    }

    #[test]
    fn treats_the_decoder_warning_for_a_truncated_scan_as_an_error() {
        let full = baseline_jpeg();
        let truncated = &full[..full.len() * 2 / 3];
        let err = candidates(truncated).unwrap_err();
        assert!(err.contains("damaged"), "{err}");
        assert!(optimize(truncated).is_err());
    }

    #[test]
    fn accepts_the_same_jpeg_when_it_is_complete() {
        // The counterpart of the test above: no warning, so no error.
        assert!(candidates(&baseline_jpeg()).is_ok());
    }

    /// Format libjpeg message `code` with `text` as its `%s` parameter, or
    /// with `number` as its first `%d`, through `libjpeg_message`.
    fn format_message(code: c_int, text: &str, number: c_int) -> String {
        assert!(text.len() < 80, "the string parameter holds 80 bytes");
        let mut err = error_manager();
        err.msg_code = code;
        // SAFETY: `s()` and `i()` view the 80-byte parameter union; the text
        // and its NUL fit (checked above), and one integer fits.
        unsafe {
            let s = &mut *err.msg_parm.s();
            for (slot, byte) in s.iter_mut().zip(text.bytes().chain([0])) {
                *slot = byte as i8;
            }
            if text.is_empty() {
                (*err.msg_parm.i())[0] = number;
            }
        }
        // SAFETY: all-zero is a valid `jpeg_common_struct` (null pointers).
        let mut common: jpeg_common_struct = unsafe { mem::zeroed() };
        common.err = &mut err;
        // SAFETY: `err` was set up by `error_manager` and outlives the call;
        // `format_message` reads nothing from the common struct but `err`.
        unsafe { libjpeg_message(&mut common) }
    }

    #[test]
    fn formats_a_libjpeg_message_longer_than_80_bytes_in_full() {
        // "Failed to create temporary file %s" with a 79-byte name: 111
        // bytes, which libjpeg writes in full (it allows JMSG_LENGTH_MAX).
        let name = "x".repeat(79);
        let message = format_message(mozjpeg_sys::JERR_TFILE_CREATE, &name, 0);
        assert_eq!(message, format!("Failed to create temporary file {name}"));
        assert!(message.len() > 80);
        assert!(message.len() < JMSG_LENGTH_MAX);
    }

    #[test]
    fn formats_a_short_libjpeg_message_exactly() {
        let message = format_message(mozjpeg_sys::JERR_BAD_PRECISION, "", 12);
        assert_eq!(message, "Unsupported JPEG data precision 12");
    }

    #[test]
    fn turns_a_libjpeg_warning_into_an_error_value() {
        // Garbage after an APP0: libjpeg skips it with a warning
        // ("extraneous bytes"), which goes through `emit_message`.
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        bytes.extend_from_slice(&[0x5A; 64]);
        let err = candidates(&bytes).unwrap_err();
        assert!(err.starts_with("the JPEG data is damaged"), "{err}");
    }

    /// A 16x16 greyscale header whose SOF declares `bits` of precision.
    fn header_with_precision(bits: u8) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
        bytes.extend_from_slice(&[1; 64]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, bits, 0x00, 0x10, 0x00, 0x10]);
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xD9]);
        bytes
    }

    #[test]
    fn turns_a_libjpeg_error_exit_into_an_error_value() {
        // 12-bit precision is not built in: `jpeg_read_header` raises
        // JERR_BAD_PRECISION through `error_exit`, not a warning.
        let err = candidates(&header_with_precision(12)).unwrap_err();
        assert_eq!(
            err,
            "could not read the JPEG data: Unsupported JPEG data precision 12"
        );
    }

    #[test]
    fn an_8_bit_header_passes_the_precision_check() {
        // The counterpart: the same header at 8 bits fails later, for its
        // token scan, and not for its precision.
        let err = candidates(&header_with_precision(8)).unwrap_err();
        assert!(!err.contains("precision"), "{err}");
    }

    #[test]
    fn caps_the_memory_of_both_libjpeg_objects() {
        let (decompressor, compressor) = run_libjpeg(|| {
            let mut decompress = Decompress::new();
            let mut compress = Compress::new();
            // SAFETY: `mem` is set by `jpeg_create_*` and lives until drop.
            unsafe {
                (
                    (*decompress.cinfo().common.mem).max_memory_to_use,
                    (*compress.cinfo().common.mem).max_memory_to_use,
                )
            }
        })
        .unwrap();
        assert_eq!(decompressor, MAX_LIBJPEG_MEMORY);
        assert_eq!(compressor, MAX_LIBJPEG_MEMORY);
    }

    #[test]
    fn refuses_a_header_over_the_pixel_limit_before_reading_coefficients() {
        // 20000x20000 greyscale: 400 MP, a header of a few bytes. libjpeg
        // accepts the dimensions, so only the pixel limit refuses it.
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
        bytes.extend_from_slice(&[1; 64]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x4E, 0x20, 0x4E, 0x20]);
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xD9]);
        let err = candidates(&bytes).unwrap_err();
        assert_eq!(err, "image too large: 20000×20000 exceeds 100 megapixels");
    }

    #[test]
    fn passes_a_header_within_the_pixel_limit_on_to_libjpeg() {
        // The same header at 64x64: not refused for size, but for its
        // missing scan data.
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
        bytes.extend_from_slice(&[1; 64]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x40, 0x00, 0x40]);
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xD9]);
        let err = candidates(&bytes).unwrap_err();
        assert!(!err.contains("image too large"), "{err}");
    }

    #[test]
    fn run_libjpeg_returns_a_libjpeg_failure_as_an_error() {
        let result: Result<(), String> = run_libjpeg(|| fail("broken".to_owned()));
        assert_eq!(result, Err("broken".to_owned()));
        assert_eq!(run_libjpeg(|| 7), Ok(7));
    }

    #[test]
    fn run_libjpeg_does_not_swallow_a_panic_that_is_not_a_libjpeg_failure() {
        let outer = std::panic::catch_unwind(|| run_libjpeg(|| -> () { panic!("a bug") }));
        assert!(outer.is_err());
    }
}
