//! Crafted, hostile input across formats, through the public API only.
//!
//! Every refusal here must be an error that leaves no output file and no
//! temporary file, and comes back quickly: a refusal that first allocates for
//! the declared image is the defect these tests exist to catch. For SVG the
//! same holds for entity expansion and nesting, and an SVG must never make
//! the optimiser open a file or a connection it names.
//!
//! Some input still crashes a library outright (a stack overflow aborts the
//! process, and cannot be caught). Those files go through the batch runner,
//! which optimises each file in its own worker process: the crash must fail
//! only that file. Never pass them to `optimize_file` here, in the test
//! process itself.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lunaepix_core::{Compression, Note, Options, Quality, WriteMode};
use lunaepix_core::{
    FileResult, MAX_FILE_BYTES, MAX_SVG_DEPTH, MAX_SVG_NODES, OptimizeError, Outcome, RunError,
    Skip, Worker, optimize_file, plan_files, run,
};
use tempfile::TempDir;

mod common;
use common::webp_fixtures as fx;

/// Today's run: lossless, replacing an earlier output.
const LOSSLESS: Options = Options {
    compression: Compression::Lossless,
    write: WriteMode::Replace,
};

/// Generous: a refusal takes milliseconds; allocating and touching gigabytes
/// takes far longer.
const QUICK: Duration = Duration::from_secs(5);

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn listing(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

/// Optimise `bytes` as `name` and assert the hostile-input contract: an
/// `Invalid` error, quickly, with no output and nothing else left behind.
/// Returns the error text for further checks.
fn assert_refused(name: &str, bytes: &[u8]) -> String {
    assert_refused_in(&LOSSLESS, name, bytes)
}

/// [`assert_refused`] in a run with `options`.
fn assert_refused_in(options: &Options, name: &str, bytes: &[u8]) -> String {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), name, bytes);
    let dest = tmp.path().join(format!("optimized-{name}"));
    let before = listing(tmp.path());

    let started = Instant::now();
    let result = optimize_file(&src, &dest, options);
    let elapsed = started.elapsed();

    let err = result.expect_err("hostile input must be refused");
    assert!(elapsed < QUICK, "took {elapsed:?}");
    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists(), "an output file was written");
    assert_eq!(listing(tmp.path()), before, "something was left behind");
    err.to_string()
}

// --- Any format: a file that grew after it was planned ---------------------------

/// A file of `len` bytes, all zero, without writing them (a sparse file on
/// the systems that have them).
fn sized_file(dir: &Path, name: &str, len: u64) -> PathBuf {
    let path = dir.join(name);
    fs::File::create(&path).unwrap().set_len(len).unwrap();
    path
}

#[test]
fn refuses_a_source_that_grew_past_the_size_limit_after_planning() {
    // The planner skips a file over the limit, but a file can grow between
    // planning and the worker reading it. The worker reads one byte past the
    // limit at most, and refuses with the planner's own reason.
    let tmp = TempDir::new().unwrap();
    let src = sized_file(tmp.path(), "grown.png", MAX_FILE_BYTES + 1);
    let dest = tmp.path().join("optimized-grown.png");

    let started = Instant::now();
    let err = optimize_file(&src, &dest, &LOSSLESS).unwrap_err();

    assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
    assert_eq!(err.reason(), Skip::TooLarge.to_string());
    assert!(
        matches!(&err, OptimizeError::Invalid { path, .. } if path == &src),
        "{err:?}"
    );
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), [OsString::from("grown.png")]);
}

#[test]
fn reads_a_source_of_exactly_the_size_limit_and_hands_it_to_the_optimiser() {
    // The counterpart: at the limit the bytes are read and decoded, so the
    // refusal is the decoder's (zeros are not a PNG), not the size reason.
    let tmp = TempDir::new().unwrap();
    let src = sized_file(tmp.path(), "full.png", MAX_FILE_BYTES);
    let dest = tmp.path().join("optimized-full.png");

    let err = optimize_file(&src, &dest, &LOSSLESS).unwrap_err();

    assert_ne!(err.reason(), Skip::TooLarge.to_string());
    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
}

// --- PNG ---------------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// One chunk with a valid CRC.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    out
}

/// IHDR data: `w`x`h`, the given bit depth and colour type, no interlace.
fn ihdr(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
    let mut data = w.to_be_bytes().to_vec();
    data.extend_from_slice(&h.to_be_bytes());
    data.extend_from_slice(&[depth, colour, 0, 0, 0]);
    chunk(b"IHDR", &data)
}

/// A zlib stream of one empty stored block (header, block, Adler-32 of
/// nothing): a token IDAT.
const TOKEN_IDAT: [u8; 11] = [
    0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
];

fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    for c in chunks {
        bytes.extend_from_slice(c);
    }
    bytes.extend(chunk(b"IEND", &[]));
    bytes
}

/// The concatenated IDAT data of a PNG.
fn idat_data(png: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        if &png[at + 4..at + 8] == b"IDAT" {
            data.extend_from_slice(&png[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    data
}

/// A normal 64x64 greyscale PNG from the `png` encoder, with a tEXt chunk
/// after the IHDR and a wasteful compression level.
fn normal_png_with_text() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 64, 64);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::FilterType::NoFilter);
    encoder
        .add_text_chunk("Comment".to_string(), "x".repeat(500))
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    let pixels: Vec<u8> = (0..64 * 64).map(|i| (i % 7 * 30) as u8).collect();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

#[test]
fn the_chunk_helper_writes_the_crc_png_requires() {
    // Proof that the crafted files carry real CRCs: IEND's CRC is fixed.
    assert_eq!(&chunk(b"IEND", &[])[8..], [0xAE, 0x42, 0x60, 0x82]);
}

#[test]
fn still_optimises_a_normal_png_with_an_ancillary_chunk_after_its_ihdr() {
    // The positive for the chunk walk: a well-formed file is not refused.
    let tmp = TempDir::new().unwrap();
    let original = normal_png_with_text();
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");

    optimize_file(&src, &dest, &LOSSLESS).unwrap();

    assert!(fs::metadata(&dest).unwrap().len() < original.len() as u64);
}

#[test]
fn refuses_a_png_that_declares_65535_by_65535_pixels() {
    let bomb = png(&[ihdr(65_535, 65_535, 8, 6), chunk(b"IDAT", &TOKEN_IDAT)]);
    assert!(bomb.len() < 100, "the fixture must be tiny");

    let message = assert_refused("bomb.png", &bomb);

    assert!(
        message.contains("image too large: 65535×65535 exceeds 100 megapixels"),
        "{message}"
    );
}

/// Huge, but 1-bit greyscale: 537 MB of raw data. Before the chunk walk
/// existed, oxipng allocated that much for these files (and survived it,
/// lazily); in 8-bit RGBA the RED run would have aborted on 17 GB.
fn huge_ihdr() -> Vec<u8> {
    ihdr(65_535, 65_535, 1, 0)
}

#[test]
fn refuses_a_png_that_hides_a_huge_ihdr_behind_an_ancillary_chunk() {
    let bomb = png(&[
        chunk(b"tEXt", b"Comment\0first"),
        huge_ihdr(),
        chunk(b"IDAT", &TOKEN_IDAT),
    ]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("first chunk is not IHDR"), "{message}");
}

#[test]
fn refuses_a_png_with_a_second_huge_ihdr_after_a_small_one() {
    // oxipng keeps the last IHDR it sees, so the small one is only a decoy.
    let bomb = png(&[ihdr(1, 1, 8, 0), huge_ihdr(), chunk(b"IDAT", &TOKEN_IDAT)]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("more than one IHDR"), "{message}");
}

#[test]
fn refuses_a_png_whose_idat_inflates_far_past_its_small_ihdr() {
    // A 16x16 IHDR (272 bytes of raw data) over the IDAT of a 2048x2048
    // image (4 MiB of zeros, a few KiB compressed). oxipng sizes its inflate
    // buffer from the IHDR and stops when the stream overflows it, so the
    // stream's own size is never allocated.
    let mut big = Vec::new();
    let mut encoder = png::Encoder::new(&mut big, 2048, 2048);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&vec![0; 2048 * 2048]).unwrap();
    writer.finish().unwrap();
    let stream = idat_data(&big);
    assert!(stream.len() < 64 * 1024, "the stream must be small");
    let bomb = png(&[ihdr(16, 16, 8, 0), chunk(b"IDAT", &stream)]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("could not read the PNG data"), "{message}");
}

// --- JPEG ---------------------------------------------------------------------------

/// A hand-made baseline JPEG header that declares `w`x`h` greyscale pixels:
/// SOI, one quantisation table, SOF0, SOS, a few bytes of scan, EOI.
fn jpeg_declaring(w: u16, h: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8];
    // DQT: table 0, 8-bit, all ones.
    bytes.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
    bytes.extend_from_slice(&[1; 64]);
    // SOF0: precision 8, height, width, one component (id 1, 1x1, table 0).
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    // SOS: one component, tables 0/0, full spectrum.
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    bytes.extend_from_slice(&[0x00; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD9]);
    bytes
}

#[test]
fn the_handmade_jpeg_header_declares_the_dimensions_it_is_given() {
    // Proof that the fixture says what the tests below rely on: the SOF0
    // payload starts 4 bytes into the segment, after the precision byte.
    let header = jpeg_declaring(65_500, 1234);
    let sof = header.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    assert_eq!(header[sof + 4], 8, "precision");
    assert_eq!(
        u16::from_be_bytes([header[sof + 5], header[sof + 6]]),
        1234,
        "height"
    );
    assert_eq!(
        u16::from_be_bytes([header[sof + 7], header[sof + 8]]),
        65_500,
        "width"
    );
}

#[test]
fn refuses_a_jpeg_that_declares_65500_by_65500_pixels() {
    // 65500 is the largest side libjpeg accepts itself, so only the pixel
    // limit can refuse this one.
    let bomb = jpeg_declaring(65_500, 65_500);
    assert!(bomb.len() < 200, "the fixture must be tiny");

    let message = assert_refused("bomb.jpg", &bomb);

    assert!(
        message.contains("image too large: 65500×65500 exceeds 100 megapixels"),
        "{message}"
    );
}

#[test]
fn refuses_a_jpeg_that_declares_65535_by_65535_pixels() {
    // libjpeg refuses a side over 65500 itself, before the pixel limit.
    let message = assert_refused("bomb.jpg", &jpeg_declaring(65_535, 65_535));

    assert!(
        message.contains("could not read the JPEG data"),
        "{message}"
    );
}

// --- SVG ----------------------------------------------------------------------------

const SVG_NS: &str = r#"xmlns="http://www.w3.org/2000/svg""#;

/// Optimise `svg` as `a.svg` in a fresh folder; the output and its folder.
fn optimise_svg(svg: &str) -> (Result<Outcome, OptimizeError>, PathBuf, TempDir) {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", svg.as_bytes());
    let dest = tmp.path().join("optimized-a.svg");
    (optimize_file(&src, &dest, &LOSSLESS), dest, tmp)
}

/// Optimise `svg`, which must succeed, and return the output as text after
/// checking that it parses as an SVG.
fn optimised_text(svg: &str) -> String {
    let (result, dest, _tmp) = optimise_svg(svg);
    result.expect("a normal SVG must optimise");
    let text = fs::read_to_string(dest).unwrap();
    let doc = roxmltree::Document::parse(&text).expect("output is XML");
    assert_eq!(doc.root_element().tag_name().name(), "svg");
    text
}

/// An Illustrator export of the older kind: a DOCTYPE with an internal subset
/// of namespace and style entities, entity references in attributes, and
/// Illustrator's private data.
const ILLUSTRATOR: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<!-- Generator: Adobe Illustrator 16.0.0, SVG Export Plug-In . SVG Version: 6.00 Build 0)  -->
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
	<!ENTITY ns_extend "http://ns.adobe.com/Extensibility/1.0/">
	<!ENTITY ns_ai "http://ns.adobe.com/AdobeIllustrator/10.0/">
	<!ENTITY ns_graphs "http://ns.adobe.com/Graphs/1.0/">
	<!ENTITY ns_vars "http://ns.adobe.com/Variables/1.0/">
	<!ENTITY ns_imrep "http://ns.adobe.com/ImageReplacement/1.0/">
	<!ENTITY ns_sfw "http://ns.adobe.com/SaveForWeb/1.0/">
	<!ENTITY ns_custom "http://ns.adobe.com/GenericCustomNamespace/1.0/">
	<!ENTITY ns_adobe_xpath "http://ns.adobe.com/XPath/1.0/">
	<!ENTITY st0 "fill:#E30613;">
	<!ENTITY st1 "fill:#FFFFFF;stroke:#1D1D1B;stroke-width:2;stroke-miterlimit:10;">
]>
<svg version="1.1" id="Layer_1" xmlns:x="&ns_extend;" xmlns:i="&ns_ai;" xmlns:graph="&ns_graphs;"
	 xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" x="0px" y="0px" width="100px"
	 height="100px" viewBox="0 0 100 100" enable-background="new 0 0 100 100" xml:space="preserve">
<switch>
	<foreignObject requiredExtensions="&ns_ai;" x="0" y="0" width="1" height="1">
		<i:pgfRef  xlink:href="#adobe_illustrator_pgf">
		</i:pgfRef>
	</foreignObject>
	<g i:extraneous="self">
		<rect x="10.000" y="10.000" style="&st0;" width="80.000" height="80.000"/>
		<circle style="&st1;" cx="50.000" cy="50.000" r="25.000"/>
	</g>
</switch>
<i:pgf  id="adobe_illustrator_pgf">
	<![CDATA[
	eJzLSM3JyVcozy/KSQEAGgQEXQ==
	]]>
</i:pgf>
</svg>
"##;

// Entity expansion.

#[test]
fn still_optimises_an_illustrator_svg_with_a_doctype_and_internal_entities() {
    let text = optimised_text(ILLUSTRATOR);
    assert!(text.len() < ILLUSTRATOR.len());
    // The style entities were expanded, not dropped.
    let lower = text.to_ascii_lowercase();
    assert!(lower.contains("e30613"), "{text}");
    assert!(lower.contains("1d1d1b"), "{text}");
}

#[test]
fn still_optimises_an_svg_whose_entities_reference_a_few_other_entities() {
    let svg = format!(
        "<!DOCTYPE svg [<!ENTITY red \"#E30613\"><!ENTITY st0 \"fill:&red;;\">]>\
         <svg {SVG_NS} viewBox=\"0 0 10 10\"><rect style=\"&st0;\" width=\"5.000\" height=\"5.000\"/></svg>"
    );
    let text = optimised_text(&svg);
    assert!(text.to_ascii_lowercase().contains("e30613"), "{text}");
}

#[test]
fn refuses_a_billion_laughs_svg() {
    // Ten levels of ten references each: 10^9 copies of "lol". roxmltree
    // itself refuses more than 255 references inside one reference.
    let mut decls = String::from("<!ENTITY l0 \"lol\">");
    for i in 1..10 {
        let refs = format!("&l{};", i - 1).repeat(10);
        decls.push_str(&format!("<!ENTITY l{i} \"{refs}\">"));
    }
    let svg = format!("<!DOCTYPE svg [{decls}]><svg {SVG_NS}><desc>&l9;</desc></svg>");
    assert!(svg.len() < 1024, "the fixture must be tiny");

    let message = assert_refused("laughs.svg", svg.as_bytes());

    assert!(message.contains("entity reference loop"), "{message}");
}

#[test]
fn refuses_an_svg_with_many_references_to_a_nested_entity() {
    // Within roxmltree's own limits per reference (250 inner references),
    // but 4000 of them: 10 GB of text from a 23 KB file.
    let decls = format!(
        "<!ENTITY a \"{}\"><!ENTITY b \"{}\">",
        "x".repeat(10_000),
        "&a;".repeat(250)
    );
    let svg = format!(
        "<!DOCTYPE svg [{decls}]><svg {SVG_NS}><desc>{}</desc></svg>",
        "&b;".repeat(4000)
    );
    assert!(svg.len() < 32 * 1024, "the fixture must be small");

    let message = assert_refused("quadratic.svg", svg.as_bytes());

    assert!(message.contains("entity references"), "{message}");
}

#[test]
fn refuses_an_svg_with_many_references_to_one_large_flat_entity() {
    // No nesting at all: 2000 references to 50 KB is 100 MB of text.
    let svg = format!(
        "<!DOCTYPE svg [<!ENTITY a \"{}\">]><svg {SVG_NS}><desc>{}</desc></svg>",
        "x".repeat(50_000),
        "&a;".repeat(2000)
    );

    let message = assert_refused("flat.svg", svg.as_bytes());

    assert!(message.contains("entity references"), "{message}");
}

// External entities and references.

/// Contents of a file the SVGs below point at. It must never reach the
/// output or an error message.
const SECRET: &str = "LUNAEPIX-SECRET-7f3a91";

fn secret_file(dir: &Path) -> PathBuf {
    write(dir, "secret.txt", SECRET.as_bytes())
}

#[test]
fn refuses_an_svg_that_references_an_external_entity_and_never_includes_it() {
    let dir = TempDir::new().unwrap();
    let secret = secret_file(dir.path());
    let uri = format!("file://{}", secret.display());
    let general = format!(
        "<!DOCTYPE svg [<!ENTITY x SYSTEM \"{uri}\">]><svg {SVG_NS}><desc>&x;</desc></svg>"
    );
    let parameter = format!("<!DOCTYPE svg [<!ENTITY % x SYSTEM \"{uri}\"> %x;]><svg {SVG_NS}/>");

    for svg in [general, parameter] {
        let message = assert_refused("xxe.svg", svg.as_bytes());
        assert!(!message.contains(SECRET), "{message}");
    }
}

#[test]
fn optimises_an_svg_that_only_declares_an_external_entity_and_leaves_it_out() {
    let dir = TempDir::new().unwrap();
    let secret = secret_file(dir.path());
    let svg = format!(
        "<!DOCTYPE svg [<!ENTITY x SYSTEM \"file://{}\">]>\
         <svg {SVG_NS}><rect width=\"5.000\" height=\"5.000\"/></svg>",
        secret.display()
    );

    let text = optimised_text(&svg);

    assert!(!text.contains(SECRET), "{text}");
}

/// How long a job may take before it counts as blocked on the trap. The trap
/// is a named pipe, so only the unix tests that use it need this.
#[cfg(unix)]
const TRAP_WAIT: Duration = Duration::from_secs(2);

/// A named pipe that nothing writes to: opening it for reading blocks until
/// a writer comes, so a job that tries to read it cannot finish.
#[cfg(unix)]
fn trap(dir: &Path) -> PathBuf {
    let path = dir.join("trap.svg");
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo failed");
    path
}

/// Run `job` on its own thread. `Some(result)` if it finishes within
/// [`TRAP_WAIT`], which proves it never opened `trap` for reading; `None` if
/// it is still blocked. Then a writer opens the trap, so a reader stuck in
/// it can finish instead of hanging the test run.
#[cfg(unix)]
fn without_opening<T: Send + 'static>(
    trap: &Path,
    job: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(job());
    });
    match rx.recv_timeout(TRAP_WAIT) {
        Ok(result) => Some(result),
        Err(_) => {
            let trap = trap.to_path_buf();
            // Blocks until a reader is there, so it gets its own thread.
            thread::spawn(move || drop(fs::OpenOptions::new().write(true).open(trap)));
            None
        }
    }
}

#[cfg(unix)]
#[test]
fn the_file_trap_catches_a_job_that_reads_the_file() {
    // Proof that the trap can spring.
    let dir = TempDir::new().unwrap();
    let trap = trap(dir.path());
    let reader = trap.clone();
    assert!(without_opening(&trap, move || fs::read(reader)).is_none());
}

#[cfg(unix)]
#[test]
fn never_opens_the_file_an_external_entity_names() {
    let dir = TempDir::new().unwrap();
    let trap = trap(dir.path());
    let svg = format!(
        "<!DOCTYPE svg [<!ENTITY x SYSTEM \"file://{}\">]><svg {SVG_NS}><desc>&x;</desc></svg>",
        trap.display()
    );

    let result = without_opening(&trap, move || {
        optimise_svg(&svg).0.map_err(|e| e.to_string())
    });

    let message = result
        .expect("must finish without opening the file")
        .expect_err("a reference to an external entity is refused");
    // Refused because the entity has no content, not for another reason.
    assert!(
        message.contains("unknown entity reference 'x'"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn never_opens_a_file_that_an_image_or_use_element_points_at() {
    let dir = TempDir::new().unwrap();
    let trap = trap(dir.path());
    let svg = format!(
        "<svg {SVG_NS} xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 10 10\">\
         <image href=\"file://{path}\" width=\"5\" height=\"5\"/>\
         <image xlink:href=\"trap.svg\" width=\"5\" height=\"5\"/>\
         <use href=\"trap.svg#a\"/><use xlink:href=\"file://{path}#b\"/></svg>",
        path = trap.display()
    );

    let result = without_opening(&trap, move || optimise_svg(&svg).0.is_ok());

    assert_eq!(
        result,
        Some(true),
        "must finish, optimised, without opening it"
    );
}

/// A listener on a free local port that accepts without blocking.
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

fn assert_nobody_connected(listener: &TcpListener) {
    match listener.accept() {
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        other => panic!("something connected: {other:?}"),
    }
}

#[test]
fn the_connection_check_sees_a_connection() {
    // Proof that `assert_nobody_connected` can fail.
    let (listener, _) = listener();
    let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    thread::sleep(Duration::from_millis(50));
    assert!(listener.accept().is_ok());
}

#[test]
fn never_connects_to_a_url_in_a_doctype_image_use_script_or_css_and_keeps_the_script() {
    let (listener, base) = listener();
    let svg = format!(
        "<!DOCTYPE svg SYSTEM \"{base}/svg11.dtd\">\
         <svg {SVG_NS} xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 10 10\">\
         <style>@import url(\"{base}/a.css\");\
         .c {{ background: url({base}/b.png); }}</style>\
         <script>fetch(\"{base}/script\")</script>\
         <image href=\"{base}/x.png\" width=\"5\" height=\"5\"/>\
         <use xlink:href=\"{base}/ext.svg#a\"/>\
         <rect class=\"c\" style=\"fill:url({base}/p#x)\" width=\"5\" height=\"5\"/>\
         <rect fill=\"url({base}/q#y)\" x=\"5\" width=\"5\" height=\"5\"/></svg>"
    );

    let text = optimised_text(&svg);

    assert_nobody_connected(&listener);
    // Kept, as svgo's preset-default keeps it; the optimiser never runs it.
    assert!(text.contains("<script"), "{text}");
    // The CSS was parsed, not dropped unread: the import survives as text.
    assert!(text.contains("a.css"), "{text}");
}

// Nesting and size.

/// `<svg>` holding `groups` nested groups around one rectangle.
fn nested_svg(groups: usize) -> String {
    format!(
        "<svg {SVG_NS} viewBox=\"0 0 10 10\">{}<rect width=\"5\" height=\"5\"/>{}</svg>",
        "<g fill=\"red\">".repeat(groups),
        "</g>".repeat(groups)
    )
}

#[test]
fn refuses_an_svg_nested_100000_levels_deep_without_crashing() {
    let message = assert_refused("deep.svg", nested_svg(100_000).as_bytes());

    assert!(message.contains("nested too deeply"), "{message}");
}

#[test]
fn still_optimises_an_svg_nested_to_the_depth_limit_on_a_default_thread_stack() {
    // <svg>, MAX_SVG_DEPTH - 2 groups and the rectangle: exactly the limit.
    // On a 2 MiB thread, the default for a spawned thread.
    let svg = nested_svg(MAX_SVG_DEPTH - 2);
    let text = thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || optimised_text(&svg))
        .unwrap()
        .join()
        .unwrap();
    // oxvg collapses the groups onto the shape (a path now); the fill stays.
    assert!(text.contains(r#"fill="red""#), "{text}");
    assert!(!text.contains("<g"), "{text}");
}

#[test]
fn refuses_an_svg_with_more_nodes_than_the_limit() {
    let svg = format!(
        "<svg {SVG_NS}>{}</svg>",
        "<g/>".repeat(MAX_SVG_NODES as usize)
    );

    let message = assert_refused("wide.svg", svg.as_bytes());

    assert!(message.contains("too large"), "{message}");
}

// --- WebP ------------------------------------------------------------------------------

const NOT_WEBP: &str = "not a WebP file (the content is not WebP data)";

/// One WebP chunk with the zero pad byte of an odd size.
fn webp_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = webp_chunk_declaring(fourcc, data.len() as u32, data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A chunk header that declares `size`, then `data` as it is.
fn webp_chunk_declaring(fourcc: &[u8; 4], size: u32, data: &[u8]) -> Vec<u8> {
    let mut out = fourcc.to_vec();
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A RIFF header with the given size field, `WEBP`, then the chunks.
fn webp_riff_declaring(size: u32, chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend(chunks.concat());
    out
}

/// A WebP file whose RIFF size is right for its chunks.
fn webp_riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let size = 4 + chunks.iter().map(Vec::len).sum::<usize>() as u32;
    webp_riff_declaring(size, chunks)
}

/// A `VP8X` chunk for a `w`x`h` canvas, stored as width-1 and height-1.
fn webp_vp8x(w: u32, h: u32) -> Vec<u8> {
    let mut data = vec![0, 0, 0, 0];
    data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    webp_chunk(b"VP8X", &data)
}

/// A `VP8L` payload that declares `w`x`h`, with a few bytes behind the header.
fn webp_vp8l_payload(w: u32, h: u32) -> Vec<u8> {
    let mut data = vec![0x2F];
    data.extend_from_slice(&((w - 1) | ((h - 1) << 14)).to_le_bytes());
    data.extend_from_slice(&[0; 5]);
    data
}

/// A real lossless WebP from `image`'s encoder.
fn normal_webp() -> Vec<u8> {
    let pixels: Vec<u8> = (0..32 * 32 * 4).map(|i| (i * 7 % 251) as u8).collect();
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(&pixels, 32, 32, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

#[test]
fn still_optimises_a_normal_webp() {
    // The positive for the chunk walk: a well-formed file is not refused, and
    // what is written still decodes.
    let tmp = TempDir::new().unwrap();
    let original = normal_webp();
    let src = write(tmp.path(), "a.webp", &original);
    let dest = tmp.path().join("optimized-a.webp");

    let outcome = optimize_file(&src, &dest, &LOSSLESS).unwrap();

    assert_eq!(outcome.before, original.len() as u64);
    assert!(outcome.after <= outcome.before);
    let written = fs::read(&dest).unwrap();
    let decoded = image::load_from_memory_with_format(&written, image::ImageFormat::WebP).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (32, 32));
    assert_eq!(
        fs::read(&src).unwrap(),
        original,
        "the original is untouched"
    );
}

#[test]
fn refuses_a_webp_file_of_only_riff() {
    assert!(assert_refused("a.webp", b"RIFF").ends_with(NOT_WEBP));
    // Empty, too: no panic, the WebP message.
    assert!(assert_refused("b.webp", b"").ends_with(NOT_WEBP));
}

#[test]
fn refuses_a_webp_that_is_not_webp_data() {
    assert!(assert_refused("png.webp", &png(&[ihdr(1, 1, 8, 0)])).ends_with(NOT_WEBP));
    let mut wave = b"RIFF".to_vec();
    wave.extend_from_slice(&36u32.to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    assert!(assert_refused("wave.webp", &wave).ends_with(NOT_WEBP));
}

#[test]
fn refuses_a_webp_chunk_of_u32_max_bytes() {
    let huge = webp_chunk_declaring(b"VP8L", u32::MAX, &webp_vp8l_payload(8, 8));
    let message = assert_refused("huge.webp", &webp_riff(&[huge]));
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_riff_size_past_the_end_of_the_file() {
    let chunks = [webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8))];
    let bytes = webp_riff_declaring(u32::MAX, &chunks);
    let message = assert_refused("long.webp", &bytes);
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_cut_off_inside_its_vp8l_chunk() {
    // The chunk size runs past the end of the file: the file stops mid-chunk.
    let whole = webp_riff(&[webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8))]);
    let cut = &whole[..whole.len() - 4];
    let message = assert_refused("cut.webp", cut);
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_with_a_vp8l_payload_shorter_than_its_header() {
    let short = webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8)[..3]);
    let message = assert_refused("short.webp", &webp_riff(&[short]));
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_with_a_misplaced_vp8x() {
    let lossless = webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8));
    let message = assert_refused("late.webp", &webp_riff(&[lossless, webp_vp8x(8, 8)]));
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_with_a_second_vp8x() {
    let lossless = webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8));
    let bytes = webp_riff(&[webp_vp8x(8, 8), webp_vp8x(65_535, 65_535), lossless]);
    let message = assert_refused("twice.webp", &bytes);
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_canvas_over_the_pixel_limit() {
    let bomb = webp_riff(&[webp_vp8x(20_000, 20_000)]);
    assert!(bomb.len() < 64, "the fixture must be tiny");
    let message = assert_refused("bomb.webp", &bomb);
    assert!(
        message.contains("image too large: 20000×20000 exceeds 100 megapixels"),
        "{message}"
    );
}

#[test]
fn refuses_a_webp_canvas_at_the_pixel_limit_with_no_image_as_unreadable() {
    let message = assert_refused("empty.webp", &webp_riff(&[webp_vp8x(10_000, 10_000)]));
    assert!(
        message.contains("could not read the WebP data"),
        "{message}"
    );
    assert!(!message.contains("too large"), "{message}");
}

/// A WebP holding `exif` as its EXIF chunk, optimised: the output.
fn optimised_with_exif(exif: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let original = fx::with_chunks(&normal_webp(), &[(*b"EXIF", exif)]);
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "exif.webp", &original);
    let dest = tmp.path().join("optimized-exif.webp");

    let started = Instant::now();
    let result = optimize_file(&src, &dest, &LOSSLESS);
    assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());

    result.expect("a malformed EXIF chunk must not refuse the file");
    (original, fs::read(&dest).unwrap())
}

#[test]
fn optimises_a_webp_with_a_malformed_exif_chunk_and_drops_the_chunk() {
    let le = |v: u32| v.to_le_bytes();
    let mut header_cut = fx::EXIF_PREFIX.to_vec();
    header_cut.extend_from_slice(b"II*");
    let mut offset_past_end = fx::tiff(false, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
    offset_past_end[4..8].copy_from_slice(&le(u32::MAX));
    let mut offset_into_header = fx::tiff(true, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
    offset_into_header[4..8].copy_from_slice(&2u32.to_be_bytes());
    let mut count_too_large = fx::tiff(false, &[]);
    count_too_large[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    let wrong_type = fx::tiff(true, &[(fx::ORIENTATION, 4, 1, 6)]);
    let cases: [(&str, Vec<u8>); 6] = [
        ("empty", Vec::new()),
        ("header cut short", header_cut),
        ("IFD0 offset past the end", offset_past_end),
        ("IFD0 offset into the header", offset_into_header),
        ("entry count larger than the payload", count_too_large),
        ("Orientation of type LONG", wrong_type),
    ];
    for (what, exif) in cases {
        let (original, out) = optimised_with_exif(&exif);
        assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L"], "{what}");
        assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&original), "{what}");
    }

    // The control: the same file with a well-formed rotation keeps its EXIF.
    let rotated = fx::exif_with_orientation(6, true, false);
    let (_, out) = optimised_with_exif(&rotated);
    assert_eq!(fx::chunk_payload(&out, b"EXIF"), Some(rotated));
}

// --- Crashes, isolated by the worker process ---------------------------------------

/// About 200 KB of CSS brackets nested 100,000 deep: enough to overflow the
/// stack of the recursive CSS parser inside oxvg.
const CSS_DEPTH: usize = 100_000;

/// Nested rules in a `<style>` element: `a{a{a{…}}}`.
fn svg_with_nested_style_rules() -> String {
    format!(
        r#"<svg {SVG_NS}><style>{}{}</style><rect width="1" height="1"/></svg>"#,
        "a{".repeat(CSS_DEPTH),
        "}".repeat(CSS_DEPTH)
    )
}

/// Nested `calc()` in a `style` attribute.
fn svg_with_nested_calc_in_a_style_attribute() -> String {
    format!(
        r#"<svg {SVG_NS}><rect width="1" height="1" style="stroke-width:{}1{}"/></svg>"#,
        "calc(".repeat(CSS_DEPTH),
        ")".repeat(CSS_DEPTH)
    )
}

/// A normal SVG with CSS in both places.
fn svg_with_normal_css() -> String {
    format!(
        r#"<svg {SVG_NS} viewBox="0 0 10 10"><style>.a {{ fill: #ff0000; }}</style><rect class="a" width="10" height="10" style="stroke-width: calc(1px + 1px)"/></svg>"#
    )
}

/// A small baseline JPEG from the `image` crate's encoder.
fn normal_jpeg() -> Vec<u8> {
    let (w, h) = (48, 32);
    let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 241) as u8).collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(&pixels, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// Run [good PNG, `svg`, good JPG] through the worker binary, and check that
/// the PNG and the JPG were optimised. Returns the SVG's result and the
/// folder's listing afterwards.
fn run_between_two_good_files(svg: &str) -> (FileResult, Vec<OsString>) {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &normal_png_with_text()),
        write(tmp.path(), "b.svg", svg.as_bytes()),
        write(tmp.path(), "c.jpg", &normal_jpeg()),
    ];
    let worker = Worker::new(env!("CARGO_BIN_EXE_lunaepix-worker"));

    let mut results = run(&plan_files(&srcs), &worker, |_, _, _| {});

    assert_eq!(results.len(), 3);
    for (i, name) in [(0, "optimized-a.png"), (2, "optimized-c.jpg")] {
        assert!(
            matches!(results[i], FileResult::Optimized(_)),
            "{name}: {:?}",
            results[i]
        );
        assert!(tmp.path().join(name).is_file(), "{name}");
    }
    let svg_result = results.remove(1);
    (svg_result, listing(tmp.path()))
}

/// The crash contract: the SVG crashed, the files around it were optimised,
/// and nothing else (no `optimized-b.svg`, no temporary file) is left.
fn assert_crashed_alone(svg: &str) {
    let (result, listing) = run_between_two_good_files(svg);

    let FileResult::Failed(error) = &result else {
        panic!("expected a failure, got {result:?}");
    };
    match error {
        RunError::Crashed {
            path,
            cause,
            stderr,
        } => {
            assert!(path.ends_with("b.svg"), "{path:?}");
            // The crash is the stack overflow, not some other failure: Rust's
            // handler reports it on stderr and aborts (a signal on unix).
            assert!(stderr.contains("overflowed its stack"), "{stderr}");
            #[cfg(unix)]
            assert!(
                matches!(cause, lunaepix_core::Crash::Signal(_)),
                "{cause:?}"
            );
            #[cfg(not(unix))]
            let _ = cause;
        }
        other => panic!("expected a crash, got {other:?}"),
    }
    let message = error.to_string();
    assert!(
        message.contains("the optimiser crashed on this file"),
        "{message}"
    );
    let mut expected: Vec<OsString> = [
        "a.png",
        "b.svg",
        "c.jpg",
        "optimized-a.png",
        "optimized-c.jpg",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    expected.sort();
    assert_eq!(listing, expected);
}

#[test]
fn an_svg_whose_nested_style_rules_crash_the_optimiser_fails_alone() {
    assert_crashed_alone(&svg_with_nested_style_rules());
}

#[test]
fn an_svg_whose_nested_calc_in_a_style_attribute_crashes_the_optimiser_fails_alone() {
    assert_crashed_alone(&svg_with_nested_calc_in_a_style_attribute());
}

#[test]
fn a_normal_svg_with_css_succeeds_through_the_worker_between_two_good_files() {
    let (result, listing) = run_between_two_good_files(&svg_with_normal_css());

    assert!(matches!(result, FileResult::Optimized(_)), "{result:?}");
    assert!(listing.contains(&OsString::from("optimized-b.svg")));
}

// --- A lossy run: the same hostile files ------------------------------------------------

fn lossy() -> Options {
    Options {
        compression: Compression::Lossy(Quality::new(75).unwrap()),
        write: WriteMode::Replace,
    }
}

/// The 16x16 IHDR over the IDAT of a 2048x2048 image, as in
/// `refuses_a_png_whose_idat_inflates_far_past_its_small_ihdr`.
fn png_whose_idat_inflates_past_its_ihdr() -> Vec<u8> {
    let mut big = Vec::new();
    let mut encoder = png::Encoder::new(&mut big, 2048, 2048);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&vec![0; 2048 * 2048]).unwrap();
    writer.finish().unwrap();
    png(&[ihdr(16, 16, 8, 0), chunk(b"IDAT", &idat_data(&big))])
}

/// A 64x48 RGBA gradient, as the lossy PNG path's own tests use.
fn gradient_png() -> Vec<u8> {
    let (w, h) = (64u32, 48u32);
    let mut pixels = Vec::new();
    for y in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(&[
                (x * 255 / (w - 1)) as u8,
                (y * 255 / (h - 1)) as u8,
                ((x + y) * 255 / (w + h - 2)) as u8,
                255,
            ]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// Every hostile PNG above, with a part of the reason it must be refused
/// with.
fn hostile_pngs() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let full = gradient_png();
    let mut damaged = full.clone();
    let middle = damaged.len() / 2;
    for byte in &mut damaged[middle..middle + 16] {
        *byte ^= 0xFF;
    }
    vec![
        (
            "declares 65535x65535",
            png(&[ihdr(65_535, 65_535, 8, 6), chunk(b"IDAT", &TOKEN_IDAT)]),
            "image too large: 65535×65535 exceeds 100 megapixels",
        ),
        (
            "a huge IHDR behind an ancillary chunk",
            png(&[
                chunk(b"tEXt", b"Comment\0first"),
                huge_ihdr(),
                chunk(b"IDAT", &TOKEN_IDAT),
            ]),
            "first chunk is not IHDR",
        ),
        (
            "a second huge IHDR",
            png(&[ihdr(1, 1, 8, 0), huge_ihdr(), chunk(b"IDAT", &TOKEN_IDAT)]),
            "more than one IHDR",
        ),
        (
            "cut off at two thirds",
            full[..full.len() * 2 / 3].to_vec(),
            "could not read the PNG data",
        ),
        (
            "cut off after 60 bytes",
            full[..60].to_vec(),
            "could not read the PNG data",
        ),
        ("damaged image data", damaged, "could not read the PNG data"),
    ]
}

#[test]
fn every_hostile_png_is_refused_in_a_lossy_run_too() {
    for (what, bytes, reason) in hostile_pngs() {
        let message = assert_refused_in(&lossy(), "bomb.png", &bytes);
        assert!(message.contains(reason), "{what}: {message}");
    }
}

#[test]
fn a_png_whose_idat_inflates_past_its_ihdr_stays_within_its_ihdr_in_a_lossy_run() {
    // The lossless path refuses this file (oxipng's inflate overflows its
    // IHDR-sized buffer). The lossy path's decoder reads only the 16x16 rows
    // the IHDR declares and stops, so it is not refused: whether it should
    // be is an open question for the controller (see the Task 8a report).
    // What must hold either way: quick, no panic, and never the stream's
    // 2048x2048.
    let tmp = TempDir::new().unwrap();
    let src = write(
        tmp.path(),
        "bomb.png",
        &png_whose_idat_inflates_past_its_ihdr(),
    );
    let dest = tmp.path().join("optimized-bomb.png");

    let started = Instant::now();
    let result = optimize_file(&src, &dest, &lossy());

    assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
    if result.is_ok() {
        let written = fs::read(&dest).unwrap();
        let decoded =
            image::load_from_memory_with_format(&written, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    } else {
        assert!(!dest.exists());
    }
}

#[test]
fn every_hostile_jpg_is_refused_in_a_lossy_run_too() {
    let message = assert_refused_in(&lossy(), "bomb.jpg", &jpeg_declaring(65_500, 65_500));
    assert!(
        message.contains("image too large: 65500×65500 exceeds 100 megapixels"),
        "{message}"
    );
    let message = assert_refused_in(&lossy(), "bomb.jpg", &jpeg_declaring(65_535, 65_535));
    assert!(
        message.contains("could not read the JPEG data"),
        "{message}"
    );
}

/// Every hostile WebP above, with a part of the reason it must be refused
/// with.
fn hostile_webps() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    const UNREADABLE: &str = "could not read the WebP data";
    let mut wave = b"RIFF".to_vec();
    wave.extend_from_slice(&36u32.to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    let lossless_8 = || webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8));
    let whole = webp_riff(&[lossless_8()]);
    vec![
        ("only RIFF", b"RIFF".to_vec(), NOT_WEBP),
        ("empty", Vec::new(), NOT_WEBP),
        ("a PNG", png(&[ihdr(1, 1, 8, 0)]), NOT_WEBP),
        ("a WAVE file", wave, NOT_WEBP),
        (
            "a chunk of u32::MAX bytes",
            webp_riff(&[webp_chunk_declaring(
                b"VP8L",
                u32::MAX,
                &webp_vp8l_payload(8, 8),
            )]),
            UNREADABLE,
        ),
        (
            "a RIFF size past the end",
            webp_riff_declaring(u32::MAX, &[lossless_8()]),
            UNREADABLE,
        ),
        (
            "cut inside VP8L",
            whole[..whole.len() - 4].to_vec(),
            UNREADABLE,
        ),
        (
            "a VP8L payload shorter than its header",
            webp_riff(&[webp_chunk(b"VP8L", &webp_vp8l_payload(8, 8)[..3])]),
            UNREADABLE,
        ),
        (
            "a misplaced VP8X",
            webp_riff(&[lossless_8(), webp_vp8x(8, 8)]),
            UNREADABLE,
        ),
        (
            "a second VP8X",
            webp_riff(&[webp_vp8x(8, 8), webp_vp8x(65_535, 65_535), lossless_8()]),
            UNREADABLE,
        ),
        (
            "a canvas over the pixel limit",
            webp_riff(&[webp_vp8x(20_000, 20_000)]),
            "image too large: 20000×20000 exceeds 100 megapixels",
        ),
        (
            "a canvas at the limit with no image",
            webp_riff(&[webp_vp8x(10_000, 10_000)]),
            UNREADABLE,
        ),
    ]
}

#[test]
fn every_hostile_webp_is_refused_in_a_lossy_run_too() {
    for (what, bytes, reason) in hostile_webps() {
        let message = assert_refused_in(&lossy(), "bomb.webp", &bytes);
        assert!(message.contains(reason), "{what}: {message}");
        if what == "a canvas at the limit with no image" {
            assert!(!message.contains("too large"), "{what}: {message}");
        }
    }
}

#[test]
fn a_source_that_grew_past_the_size_limit_is_refused_in_a_lossy_run_too() {
    let tmp = TempDir::new().unwrap();
    let src = sized_file(tmp.path(), "grown.png", MAX_FILE_BYTES + 1);
    let dest = tmp.path().join("optimized-grown.png");

    let started = Instant::now();
    let err = optimize_file(&src, &dest, &lossy()).unwrap_err();

    assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
    assert_eq!(err.reason(), Skip::TooLarge.to_string());
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), [OsString::from("grown.png")]);
}

/// Optimise `bytes` as `name` in a lossy run that must succeed, quickly:
/// the outcome and the output.
fn lossy_output(name: &str, bytes: &[u8]) -> (Outcome, Vec<u8>) {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), name, bytes);
    let dest = tmp.path().join(format!("optimized-{name}"));

    let started = Instant::now();
    let outcome = optimize_file(&src, &dest, &lossy()).expect("a normal file is not refused");

    assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
    assert_eq!(fs::read(&src).unwrap(), bytes, "the original is untouched");
    (outcome, fs::read(&dest).unwrap())
}

#[test]
fn a_normal_png_and_jpg_are_not_refused_in_a_lossy_run() {
    let (_, written) = lossy_output("a.png", &normal_png_with_text());
    let decoded = image::load_from_memory_with_format(&written, image::ImageFormat::Png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (64, 64));

    let (_, written) = lossy_output("a.jpg", &normal_jpeg());
    let decoded = image::load_from_memory_with_format(&written, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (48, 32));
}

#[test]
fn a_normal_webp_keeps_its_original_bytes_in_a_lossy_run() {
    // At 32x32 the lossy file is not smaller, so the original comes back.
    let original = normal_webp();

    let (outcome, written) = lossy_output("a.webp", &original);

    assert_eq!(outcome.note, Some(Note::OriginalKept));
    assert_eq!(written, original);
}

#[test]
fn a_webp_with_a_malformed_exif_chunk_is_not_refused_in_a_lossy_run() {
    let le = |v: u32| v.to_le_bytes();
    let mut header_cut = fx::EXIF_PREFIX.to_vec();
    header_cut.extend_from_slice(b"II*");
    let mut offset_past_end = fx::tiff(false, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
    offset_past_end[4..8].copy_from_slice(&le(u32::MAX));
    let mut offset_into_header = fx::tiff(true, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
    offset_into_header[4..8].copy_from_slice(&2u32.to_be_bytes());
    let mut count_too_large = fx::tiff(false, &[]);
    count_too_large[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    let wrong_type = fx::tiff(true, &[(fx::ORIENTATION, 4, 1, 6)]);
    let cases: [(&str, Vec<u8>); 6] = [
        ("empty", Vec::new()),
        ("header cut short", header_cut),
        ("IFD0 offset past the end", offset_past_end),
        ("IFD0 offset into the header", offset_into_header),
        ("entry count larger than the payload", count_too_large),
        ("Orientation of type LONG", wrong_type),
    ];
    for (what, exif) in cases {
        let original = fx::with_chunks(&normal_webp(), &[(*b"EXIF", &exif)]);

        let (outcome, written) = lossy_output("exif.webp", &original);

        // At 32x32 the lossy file is not smaller: the original comes back.
        assert_eq!(outcome.note, Some(Note::OriginalKept), "{what}");
        assert_eq!(written, original, "{what}");
    }
}
