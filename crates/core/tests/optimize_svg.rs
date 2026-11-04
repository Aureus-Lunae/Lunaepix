//! `optimize_file` for SVG, through the public API only. Fixtures are inline:
//! nothing depends on `samples/`.
//!
//! "Renders alike" is the user's rule (2026-11-04): svgo and oxvg both round
//! coordinates, so neither is pixel-identical. Edge noise is accepted; see
//! [`compare`] for the exact definition.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_core::{OptimizeError, optimize_file};
use resvg::{tiny_skia, usvg};
use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

/// A drawing as an editor saves it: XML declaration, comments, editor
/// namespaces and attributes, RDF metadata, an unused gradient, inline styles
/// and numbers with needless digits, some past the 3 digits oxvg keeps. No
/// text, so no fonts are needed.
const DRAWING: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!-- Created with Inkscape (http://www.inkscape.org/) -->
<svg
   xmlns:dc="http://purl.org/dc/elements/1.1/"
   xmlns:cc="http://creativecommons.org/ns#"
   xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
   xmlns:svg="http://www.w3.org/2000/svg"
   xmlns="http://www.w3.org/2000/svg"
   xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
   xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
   width="200.00000"
   height="150.00000"
   viewBox="0 0 200.00000 150.00000"
   version="1.1"
   id="svg8"
   inkscape:version="1.0 (4035a4fb49, 2020-05-01)"
   sodipodi:docname="badge.svg">
  <defs
     id="defs2">
    <linearGradient
       inkscape:collect="always"
       id="unusedGradient">
      <stop
         style="stop-color:#000000;stop-opacity:1.0000000"
         offset="0.00000000"
         id="stop1" />
      <stop
         style="stop-color:#ffffff;stop-opacity:1.0000000"
         offset="1.00000000"
         id="stop2" />
    </linearGradient>
  </defs>
  <sodipodi:namedview
     id="base"
     pagecolor="#ffffff"
     bordercolor="#666666"
     borderopacity="1.0"
     inkscape:pageopacity="0.0"
     inkscape:pageshadow="2"
     inkscape:zoom="1.4"
     inkscape:cx="100.00000"
     inkscape:cy="75.000000"
     inkscape:document-units="px"
     inkscape:current-layer="layer1"
     showgrid="false"
     inkscape:window-width="1920"
     inkscape:window-height="1016" />
  <metadata
     id="metadata5">
    <rdf:RDF>
      <cc:Work
         rdf:about="">
        <dc:format>image/svg+xml</dc:format>
        <dc:type
           rdf:resource="http://purl.org/dc/dcmitype/StillImage" />
        <dc:title>Badge</dc:title>
      </cc:Work>
    </rdf:RDF>
  </metadata>
  <g
     inkscape:label="Layer 1"
     inkscape:groupmode="layer"
     id="layer1">
    <!-- the background card -->
    <rect
       style="fill:#336699;fill-opacity:1.000000;stroke:none;stroke-width:1.00000"
       id="rect10"
       width="180.00000"
       height="130.00000"
       x="10.000000"
       y="10.000000"
       rx="12.000000"
       ry="12.000000" />
    <!-- the sun -->
    <circle
       style="fill:#ffcc00;fill-opacity:1;stroke:#000000;stroke-width:2.0000000;stroke-opacity:1"
       id="path12"
       cx="100.00000"
       cy="75.0004821"
       r="40.0012345" />
    <path
       style="fill:none;stroke:#ffffff;stroke-width:6.0000000;stroke-linecap:round;stroke-linejoin:round;stroke-opacity:1"
       d="M 70.1234567,75.0006789 L 90.0004444,95.3216549 L 130.0007777,55.4440001"
       id="path14"
       inkscape:connector-curvature="0"
       sodipodi:nodetypes="ccc" />
  </g>
</svg>
"##;

fn drawing() -> Vec<u8> {
    DRAWING.as_bytes().to_vec()
}

/// Well-formed up to the point where `<g>` is closed as `</svg>`.
fn broken_xml() -> Vec<u8> {
    br#"<svg xmlns="http://www.w3.org/2000/svg"><g><rect width="1" height="1"/></svg>"#.to_vec()
}

/// A real PNG file.
fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbImage::from_raw(8, 8, vec![90; 8 * 8 * 3])
        .unwrap()
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

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

/// The bytes parse as XML and the root element is `<svg>`.
fn assert_parses_as_svg(bytes: &[u8]) {
    let text = std::str::from_utf8(bytes).expect("UTF-8");
    let doc = roxmltree::Document::parse(text).expect("well-formed XML");
    assert_eq!(doc.root_element().tag_name().name(), "svg");
}

// --- the render comparison ----------------------------------------------------------

/// The result of comparing two renders of the same size.
#[derive(Debug, PartialEq, Eq)]
struct Comparison {
    /// Pixels whose RGBA bytes differ.
    different: usize,
    /// Differing pixels that lie inside a flat area of the original.
    flat: usize,
    total: usize,
}

/// Compare two RGBA renders of `width` x `height`, `a` being the original.
///
/// A pixel is **flat** when it is not on the border of the image and all 8
/// of its neighbours in the original render have exactly its RGBA value.
/// Anti-aliasing noise from a rounded coordinate only changes pixels that a
/// shape's edge passes through, and those always have a neighbour of another
/// colour, so they are never flat. A recoloured, moved or missing shape does
/// change flat pixels.
fn compare(a: &[u8], b: &[u8], width: usize, height: usize) -> Comparison {
    assert_eq!(a.len(), width * height * 4);
    assert_eq!(b.len(), a.len());
    let px = |d: &[u8], x: usize, y: usize| {
        let i = (y * width + x) * 4;
        [d[i], d[i + 1], d[i + 2], d[i + 3]]
    };
    let mut result = Comparison {
        different: 0,
        flat: 0,
        total: width * height,
    };
    for y in 0..height {
        for x in 0..width {
            let p = px(a, x, y);
            if p == px(b, x, y) {
                continue;
            }
            result.different += 1;
            let inside = x > 0 && y > 0 && x + 1 < width && y + 1 < height;
            if inside && (y - 1..=y + 1).all(|ny| (x - 1..=x + 1).all(|nx| px(a, nx, ny) == p)) {
                result.flat += 1;
            }
        }
    }
    result
}

/// The user's rule: no differing pixel in a flat area, and at most 0.5 % of
/// the pixels different.
fn within_tolerance(c: &Comparison) -> Result<(), String> {
    if c.flat > 0 {
        return Err(format!("{} differing pixels in flat areas: {c:?}", c.flat));
    }
    if c.different * 1000 > c.total * 5 {
        return Err(format!("more than 0.5 % of the pixels differ: {c:?}"));
    }
    Ok(())
}

/// Render width. Both files are drawn at this width and the original's
/// aspect ratio, each scaled from its own intrinsic size.
const RENDER_WIDTH: u32 = 400;

fn render(svg: &[u8], width: u32, height: u32) -> Vec<u8> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).expect("usvg parses it");
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(width, height).unwrap();
    let scale = tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, scale, &mut pixmap.as_mut());
    pixmap.data().to_vec()
}

fn render_comparison(original: &[u8], candidate: &[u8]) -> Comparison {
    let size = usvg::Tree::from_data(original, &usvg::Options::default())
        .unwrap()
        .size();
    let height = (RENDER_WIDTH as f32 * size.height() / size.width()).round() as u32;
    let a = render(original, RENDER_WIDTH, height);
    let b = render(candidate, RENDER_WIDTH, height);
    compare(&a, &b, RENDER_WIDTH as usize, height as usize)
}

fn renders_alike(original: &[u8], candidate: &[u8]) -> Result<(), String> {
    within_tolerance(&render_comparison(original, candidate))
}

#[test]
fn the_drawing_renders_something_to_compare() {
    // Guard for the tests below: an empty render would compare as equal.
    let c = render_comparison(&drawing(), &drawing());
    assert_eq!(c.different, 0);
    let pixels = render(&drawing(), 200, 150);
    assert!(pixels.chunks(4).filter(|p| p[3] != 0).count() > 10_000);
}

#[test]
fn the_render_comparison_rejects_a_visibly_changed_fill() {
    let recoloured = DRAWING.replace("fill:#ffcc00", "fill:#00ccff");
    assert_ne!(recoloured, DRAWING);
    let err = renders_alike(&drawing(), recoloured.as_bytes()).unwrap_err();
    assert!(err.contains("flat areas"), "{err}");
}

#[test]
fn the_render_comparison_rejects_a_moved_shape() {
    let moved = DRAWING.replace(r#"cx="100.00000""#, r#"cx="110.00000""#);
    assert_ne!(moved, DRAWING);
    assert!(renders_alike(&drawing(), moved.as_bytes()).is_err());
}

#[test]
fn a_changed_pixel_inside_a_flat_area_is_flat() {
    // 5x5, one colour; the centre pixel differs.
    let a = vec![9u8; 5 * 5 * 4];
    let mut b = a.clone();
    b[(2 * 5 + 2) * 4] = 10;
    let c = compare(&a, &b, 5, 5);
    assert_eq!((c.different, c.flat), (1, 1));
    assert!(within_tolerance(&c).is_err());
}

#[test]
fn a_changed_pixel_beside_an_edge_or_on_the_border_is_not_flat() {
    // Left half one colour, right half another: x = 2 has a neighbour of the
    // other colour, so a change there is edge noise. So is a border pixel.
    let mut a = vec![0u8; 5 * 5 * 4];
    for y in 0..5 {
        for x in 3..5 {
            a[(y * 5 + x) * 4..(y * 5 + x) * 4 + 4].copy_from_slice(&[255; 4]);
        }
    }
    let mut b = a.clone();
    b[(2 * 5 + 2) * 4] = 1;
    b[0] = 1;
    let c = compare(&a, &b, 5, 5);
    assert_eq!((c.different, c.flat), (2, 0));
}

#[test]
fn edge_noise_on_more_than_half_a_percent_of_the_pixels_is_rejected() {
    // A checkerboard has no flat pixel at all, so only the share counts.
    let (w, h) = (100, 100);
    let board = |invert: bool| -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let on = ((i % w) + (i / w)) % 2 == 0;
                if on != invert {
                    [255u8; 4]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect()
    };
    let a = board(false);
    let mut b = a.clone();
    // 50 pixels (0.5 %) changed: within tolerance.
    for i in 0..50 {
        b[i * 4 * 2] ^= 1;
    }
    let c = compare(&a, &b, w, h);
    assert_eq!((c.different, c.flat), (50, 0));
    assert_eq!(within_tolerance(&c), Ok(()));
    // One more: over.
    b[100 * 4 * 2] ^= 1;
    let err = within_tolerance(&compare(&a, &b, w, h)).unwrap_err();
    assert!(err.contains("0.5 %"), "{err}");
}

// --- smaller, still an SVG, renders alike ----------------------------------------------

#[test]
fn optimizes_a_cruft_laden_svg_to_a_smaller_svg_that_renders_alike() {
    let tmp = TempDir::new().unwrap();
    let original = drawing();
    let src = write(tmp.path(), "badge.svg", &original);
    let dest = tmp.path().join("optimized-badge.svg");

    let outcome = optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert!(
        optimized.len() * 2 < original.len(),
        "{} is not under half of {}",
        optimized.len(),
        original.len()
    );
    assert_parses_as_svg(&optimized);
    // oxvg rounds to 3 digits, so edges shift a little (16 pixels when
    // measured): exactly the noise the tolerance exists for.
    assert_eq!(renders_alike(&original, &optimized), Ok(()));
    assert_eq!(outcome.before, original.len() as u64);
    assert_eq!(outcome.after, optimized.len() as u64);
    let text = String::from_utf8(optimized).unwrap();
    for cruft in ["Inkscape", "sodipodi", "metadata", "unusedGradient", "<!--"] {
        assert!(!text.contains(cruft), "{cruft} kept: {text}");
    }
}

#[test]
fn rejects_an_svg_that_is_not_well_formed_xml_and_leaves_no_output_or_temp_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &broken_xml());
    let dest = tmp.path().join("optimized-a.svg");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not well-formed XML"), "{err}");
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn rejects_well_formed_xml_whose_root_is_not_svg_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let html = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body/></html>"#;
    let src = write(tmp.path(), "page.svg", html);
    let dest = tmp.path().join("optimized-page.svg");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not an SVG file"), "{err}");
    assert_eq!(listing(tmp.path()), before);
}

// --- the source is never touched ------------------------------------------------

#[test]
fn keeps_the_source_bytes_unchanged_after_a_successful_run() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &drawing());
    optimize_file(&src, &tmp.path().join("optimized-a.svg")).unwrap();
    assert_eq!(fs::read(&src).unwrap(), drawing());
}

#[test]
fn keeps_the_source_unchanged_when_the_run_fails() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &broken_xml());
    let err = optimize_file(&src, &tmp.path().join("optimized-a.svg")).unwrap_err();
    assert!(err.to_string().contains("not well-formed XML"), "{err}");
    assert_eq!(fs::read(&src).unwrap(), broken_xml());
}

// --- never larger ---------------------------------------------------------------------

#[test]
fn writes_an_already_optimised_svg_as_an_equal_size_copy() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.svg", &drawing());
    let once = tmp.path().join("once.svg");
    optimize_file(&first, &once).unwrap();
    let twice = tmp.path().join("twice.svg");

    let outcome = optimize_file(&once, &twice).unwrap();

    assert_eq!(outcome.before, outcome.after);
    assert_eq!(fs::read(&once).unwrap(), fs::read(&twice).unwrap());
}

#[test]
fn keeps_the_original_bytes_when_the_optimised_svg_is_not_smaller() {
    // Already minimal: oxvg can only reformat it, never shrink it.
    let tmp = TempDir::new().unwrap();
    let minimal = br#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h9v9z"/></svg>"#;
    let src = write(tmp.path(), "a.svg", minimal);
    let dest = tmp.path().join("optimized-a.svg");

    let outcome = optimize_file(&src, &dest).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), minimal);
    assert_eq!(outcome.after, minimal.len() as u64);
}

// --- content and extension ------------------------------------------------------

#[test]
fn accepts_svg_bytes_named_with_an_uppercase_svg_extension() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "BADGE.SVG", &drawing());
    let dest = tmp.path().join("optimized-BADGE.SVG");

    optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert!(optimized.len() < drawing().len());
    assert_parses_as_svg(&optimized);
}

#[test]
fn rejects_png_bytes_named_svg_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "picture.svg", &png_bytes());
    let dest = tmp.path().join("optimized-picture.svg");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not an SVG file"), "{err}");
    assert!(!dest.exists());
    assert_eq!(fs::read(&src).unwrap(), png_bytes());
}

// --- replacing, and nothing left after a failure ----------------------------------------

#[test]
fn replaces_an_existing_optimized_svg() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &drawing());
    let dest = write(
        tmp.path(),
        "optimized-a.svg",
        b"stale output from an earlier run",
    );

    optimize_file(&src, &dest).unwrap();

    let written = fs::read(&dest).unwrap();
    assert_parses_as_svg(&written);
    assert_eq!(renders_alike(&drawing(), &written), Ok(()));
    assert_eq!(
        listing(tmp.path()).len(),
        2,
        "only the source and the output"
    );
}

#[test]
fn keeps_an_existing_optimized_svg_and_leaves_nothing_when_the_new_run_fails() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &broken_xml());
    let dest = write(
        tmp.path(),
        "optimized-a.svg",
        b"output of an earlier good run",
    );
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(err.to_string().contains("not well-formed XML"), "{err}");
    assert_eq!(fs::read(&dest).unwrap(), b"output of an earlier good run");
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn writes_an_svg_into_the_optimized_folder() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &drawing());
    let dest = tmp.path().join("optimized").join("a.svg");

    optimize_file(&src, &dest).unwrap();

    assert_eq!(
        listing(&tmp.path().join("optimized")),
        [OsString::from("a.svg")]
    );
}

#[test]
fn does_not_create_the_destination_folder_when_the_svg_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.svg", &broken_xml());
    let dest = tmp.path().join("optimized").join("a.svg");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(err.to_string().contains("not well-formed XML"), "{err}");
    assert!(!tmp.path().join("optimized").exists());
}
