//! The real `lunaepix` binary, run on real files in temporary folders. Its
//! stdout is a pipe here, so no colour is expected. Fixtures are made here.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

const SVG_NS: &str = r#"xmlns="http://www.w3.org/2000/svg""#;

/// A PNG with something to gain: a flat image, stored with fast compression.
fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 64, 64);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&vec![90; 64 * 64 * 3]).unwrap();
    writer.finish().unwrap();
    out
}

/// A normal SVG with something to gain.
fn svg_text() -> String {
    format!(
        r##"<svg {SVG_NS} viewBox="0 0 10 10"><!-- a comment --><rect width="10.000" height="10.000" fill="#ff0000"/></svg>"##
    )
}

/// About 200 KB of CSS brackets nested 100,000 deep: overflows the stack of
/// the CSS parser inside the SVG optimiser, which then aborts its process.
/// The same fixture as `crates/core/tests/hostile_input.rs`.
fn svg_with_nested_style_rules() -> String {
    const DEPTH: usize = 100_000;
    format!(
        r#"<svg {SVG_NS}><style>{}{}</style><rect width="1" height="1"/></svg>"#,
        "a{".repeat(DEPTH),
        "}".repeat(DEPTH)
    )
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn listing(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<OsString> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

fn names(list: &[&str]) -> Vec<OsString> {
    let mut names: Vec<OsString> = list.iter().map(OsString::from).collect();
    names.sort();
    names
}

/// Run `lunaepix` with these arguments, in `cwd`.
fn lunaepix(cwd: &Path, args: &[&OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lunaepix"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn run_on(cwd: &Path, args: &[&Path]) -> Output {
    let args: Vec<OsString> = args.iter().map(|path| path.as_os_str().into()).collect();
    lunaepix(cwd, &args.iter().collect::<Vec<_>>())
}

fn run_with(cwd: &Path, args: &[&str]) -> Output {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    lunaepix(cwd, &args.iter().collect::<Vec<_>>())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The stdout of a run, split into its blocks of four lines, with the number
/// of milliseconds checked and replaced by `<ms>`.
fn blocks(stdout: &str) -> Vec<[String; 4]> {
    assert!(stdout.is_empty() || stdout.ends_with("\n\n"), "{stdout:?}");
    let lines: Vec<&str> = stdout.split('\n').collect();
    // `split` leaves one empty piece after the final newline.
    let (last, lines) = lines.split_last().unwrap();
    assert_eq!(*last, "", "{stdout:?}");
    assert_eq!(lines.len() % 4, 0, "{stdout:?}");
    lines
        .chunks(4)
        .map(|block| {
            let ms = block[1]
                .strip_prefix("Done in ")
                .and_then(|rest| rest.strip_suffix(" ms!"))
                .unwrap_or_else(|| panic!("not a 'Done in' line: {:?}", block[1]));
            assert!(ms.parse::<u64>().is_ok(), "{:?}", block[1]);
            [
                block[0].to_owned(),
                "Done in <ms> ms!".to_owned(),
                block[2].to_owned(),
                block[3].to_owned(),
            ]
        })
        .collect()
}

/// The block the layout asks for, built from the sizes of the two files.
fn expected_block(name: &str, src: &Path, dest: &Path) -> [String; 4] {
    let before = fs::metadata(src).unwrap().len() as f64;
    let after = fs::metadata(dest).unwrap().len() as f64;
    [
        format!("{name}:"),
        "Done in <ms> ms!".to_owned(),
        format!(
            "{:.3} KiB - {:.1}% = {:.3} KiB",
            before / 1024.0,
            (before - after) * 100.0 / before,
            after / 1024.0
        ),
        String::new(),
    ]
}

// --- optimising -------------------------------------------------------------

#[test]
fn optimises_two_images_and_prints_the_shell_functions_layout_without_colour() {
    let tmp = TempDir::new().unwrap();
    let png = write(tmp.path(), "a.png", &png_bytes());
    let svg = write(tmp.path(), "b.svg", svg_text().as_bytes());

    let output = run_on(tmp.path(), &[&png, &svg]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(!stdout.contains('\x1b'), "{stdout:?}");
    // In input order, each block as the layout says.
    assert_eq!(
        blocks(&stdout),
        vec![
            expected_block("a.png", &png, &tmp.path().join("optimized-a.png")),
            expected_block("b.svg", &svg, &tmp.path().join("optimized-b.svg")),
        ]
    );
    assert_eq!(
        listing(tmp.path()),
        names(&["a.png", "b.svg", "optimized-a.png", "optimized-b.svg"])
    );
}

#[test]
fn a_corrupt_png_fails_alone_with_a_stderr_line_and_exit_1() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.png", &png_bytes());
    let bad = write(tmp.path(), "bad.png", b"this is not a png");
    let last = write(tmp.path(), "c.png", &png_bytes());

    let output = run_on(tmp.path(), &[&first, &bad, &last]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("lunaepix: failed (")
            && stderr.ends_with(&format!("): {}\n", bad.display())),
        "{stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    // The path is named once, at the end; the reason does not repeat it.
    assert_eq!(
        stderr.matches(&bad.display().to_string()).count(),
        1,
        "{stderr:?}"
    );
    // The good files around it are done, and printed.
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![
            expected_block("a.png", &first, &tmp.path().join("optimized-a.png")),
            expected_block("c.png", &last, &tmp.path().join("optimized-c.png")),
        ]
    );
    assert_eq!(
        listing(tmp.path()),
        names(&[
            "a.png",
            "bad.png",
            "c.png",
            "optimized-a.png",
            "optimized-c.png"
        ])
    );
}

#[test]
fn dir_writes_into_the_optimized_folder_of_that_dir() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();
    let png = write(&dir, "a.png", &png_bytes());
    let svg = write(&dir, "b.svg", svg_text().as_bytes());

    let output = run_on(tmp.path(), &[Path::new("--dir"), &dir]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let out_dir = dir.join("optimized");
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![
            expected_block("a.png", &png, &out_dir.join("a.png")),
            expected_block("b.svg", &svg, &out_dir.join("b.svg")),
        ]
    );
    assert_eq!(listing(&dir), names(&["a.png", "b.svg", "optimized"]));
    assert_eq!(listing(&out_dir), names(&["a.png", "b.svg"]));
}

#[test]
fn a_txt_file_in_the_dir_is_skipped_on_stderr_with_no_output_and_exit_0() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();
    let notes = write(&dir, "notes.txt", b"hello");

    let output = run_on(tmp.path(), &[Path::new("--dir"), &dir]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        format!(
            "lunaepix: skipped (Not a PNG, JPG, SVG or WebP file): {}\n",
            notes.display()
        )
    );
    // No file, and no `optimized/` folder either.
    assert_eq!(listing(&dir), names(&["notes.txt"]));
}

#[test]
fn a_missing_file_is_skipped_not_failed() {
    let tmp = TempDir::new().unwrap();
    let output = run_with(tmp.path(), &["missing.png"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        "lunaepix: skipped (Not a readable file): missing.png\n"
    );
}

#[test]
fn works_on_a_name_with_spaces_and_non_ascii_characters() {
    let tmp = TempDir::new().unwrap();
    let name = "my pic é ü 写真.png";
    let png = write(tmp.path(), name, &png_bytes());

    let output = run_on(tmp.path(), &[&png]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let dest = tmp.path().join(format!("optimized-{name}"));
    assert!(dest.is_file());
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![expected_block(name, &png, &dest)]
    );
}

#[cfg(unix)]
#[test]
fn works_on_a_name_that_is_not_valid_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = TempDir::new().unwrap();
    let name = OsString::from_vec(vec![b'a', 0xff, b'.', b'p', b'n', b'g']);
    let src = tmp.path().join(&name);
    fs::write(&src, png_bytes()).unwrap();

    let output = run_on(tmp.path(), &[&src]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let mut dest_name = OsString::from("optimized-");
    dest_name.push(&name);
    assert!(tmp.path().join(dest_name).is_file());
}

#[test]
fn a_double_dash_lets_a_file_name_start_with_two_dashes() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "--dir.png", &png_bytes());

    let output = run_with(tmp.path(), &["--", "--dir.png"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(tmp.path().join("optimized---dir.png").is_file());
}

// --- usage ------------------------------------------------------------------

fn assert_usage_error(output: &Output, dir: &Path, expected_listing: &[&str]) {
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    assert!(
        text(&output.stderr).starts_with("usage: lunaepix"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(listing(dir), names(expected_listing));
}

#[test]
fn no_arguments_prints_the_usage_and_exits_2() {
    let tmp = TempDir::new().unwrap();
    assert_usage_error(&run_with(tmp.path(), &[]), tmp.path(), &[]);
}

#[test]
fn an_unknown_option_prints_the_usage_and_exits_2() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", &png_bytes());
    // Nothing is optimised, even though a valid file came with it.
    assert_usage_error(
        &run_with(tmp.path(), &["--bogus", "a.png"]),
        tmp.path(),
        &["a.png"],
    );
    // The same name needs `--` to be a file.
    write(tmp.path(), "--dir.png", &png_bytes());
    assert_usage_error(
        &run_with(tmp.path(), &["--dir.png"]),
        tmp.path(),
        &["--dir.png", "a.png"],
    );
}

#[test]
fn dir_without_a_value_prints_the_usage_and_exits_2() {
    let tmp = TempDir::new().unwrap();
    assert_usage_error(&run_with(tmp.path(), &["--dir"]), tmp.path(), &[]);
}

// --- a crash in the optimiser -----------------------------------------------

#[test]
fn an_svg_that_crashes_the_optimiser_fails_alone_and_the_exit_code_is_1() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.png", &png_bytes());
    let bad = write(
        tmp.path(),
        "b.svg",
        svg_with_nested_style_rules().as_bytes(),
    );
    let last = write(tmp.path(), "c.svg", svg_text().as_bytes());

    let output = run_on(tmp.path(), &[&first, &bad, &last]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("lunaepix: failed (")
            && stderr.contains("the optimiser crashed on this file")
            && stderr.ends_with(&format!("): {}\n", bad.display())),
        "{stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    assert_eq!(
        stderr.matches(&bad.display().to_string()).count(),
        1,
        "{stderr:?}"
    );
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![
            expected_block("a.png", &first, &tmp.path().join("optimized-a.png")),
            expected_block("c.svg", &last, &tmp.path().join("optimized-c.svg")),
        ]
    );
    assert_eq!(
        listing(tmp.path()),
        names(&[
            "a.png",
            "b.svg",
            "c.svg",
            "optimized-a.png",
            "optimized-c.svg"
        ])
    );
}

#[test]
fn a_normal_svg_with_css_succeeds() {
    let tmp = TempDir::new().unwrap();
    let svg = write(
        tmp.path(),
        "css.svg",
        format!(
            r#"<svg {SVG_NS} viewBox="0 0 10 10"><style>.a {{ fill: #ff0000; }}</style><rect class="a" width="10" height="10"/></svg>"#
        )
        .as_bytes(),
    );

    let output = run_on(tmp.path(), &[&svg]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert!(tmp.path().join("optimized-css.svg").is_file());
}

// --- control characters in names ---------------------------------------------

/// No character a terminal acts on, except exactly `lines` newlines. Only the
/// unix tests below use it: Windows does not allow such characters in a name.
#[cfg(unix)]
fn assert_no_control_characters(output: &str, lines: usize) {
    assert_eq!(output.matches('\n').count(), lines, "{output:?}");
    for c in output.chars().filter(|&c| c != '\n') {
        assert!(!c.is_control(), "raw {c:?} in {output:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_file_name_with_terminal_escape_codes_is_printed_escaped() {
    let tmp = TempDir::new().unwrap();
    let hostile = "\x1b]0;pwned\x07a.png";
    let png = write(tmp.path(), hostile, &png_bytes());
    let bad = write(tmp.path(), "\x1b[2Jbad.png", b"this is not a png");
    let skipped = write(tmp.path(), "\x1b[31mnotes.txt", b"hello");

    let output = run_on(tmp.path(), &[&png, &bad, &skipped]);

    assert_eq!(output.status.code(), Some(1));
    // The success block: four lines, the name escaped, the codes visible.
    let stdout = text(&output.stdout);
    assert_no_control_characters(&stdout, 4);
    assert!(
        stdout.starts_with("\\x1b]0;pwned\\x07a.png:\nDone in "),
        "{stdout:?}"
    );
    // The failure and the skip: one line each, nothing raw.
    let stderr = text(&output.stderr);
    assert_no_control_characters(&stderr, 2);
    assert!(stderr.contains("\\x1b[2Jbad.png\n"), "{stderr:?}");
    assert!(
        stderr.contains("lunaepix: skipped (Not a PNG, JPG, SVG or WebP file): ")
            && stderr.contains("\\x1b[31mnotes.txt\n"),
        "{stderr:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_dir_name_with_terminal_escape_codes_is_printed_escaped() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("\x1b[2Jnone");

    let output = run_on(tmp.path(), &[Path::new("--dir"), &missing]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert_no_control_characters(&stderr, 1);
    assert!(stderr.ends_with("\\x1b[2Jnone\n"), "{stderr:?}");
}

#[test]
fn a_normal_non_ascii_name_is_printed_as_it_is() {
    let tmp = TempDir::new().unwrap();
    let name = "Mobilis Logo – zwart é.png";
    let png = write(tmp.path(), name, &png_bytes());

    let output = run_on(tmp.path(), &[&png]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stdout).starts_with(&format!("{name}:\n")));
}

// --- a --dir that is not a folder --------------------------------------------

#[test]
fn a_dir_that_does_not_exist_fails_with_exit_1_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("none");

    let output = run_on(tmp.path(), &[Path::new("--dir"), &missing]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        format!("lunaepix: failed (not a folder): {}\n", missing.display())
    );
    assert_eq!(listing(tmp.path()), names(&[]));
}

#[test]
fn a_dir_that_is_a_file_fails_with_exit_1_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let file = write(tmp.path(), "a.png", &png_bytes());

    let output = run_on(tmp.path(), &[Path::new("--dir"), &file]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        format!("lunaepix: failed (not a folder): {}\n", file.display())
    );
    assert_eq!(listing(tmp.path()), names(&["a.png"]));
}

#[test]
fn an_existing_empty_dir_exits_0_with_no_output() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();

    let output = run_on(tmp.path(), &[Path::new("--dir"), &dir]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(text(&output.stderr), "");
    // No `optimized/` folder is made for nothing.
    assert_eq!(listing(&dir), names(&[]));
}

// --- --lossy, --quality and --out -------------------------------------------

use std::ffi::OsStr;

use lunaepix_core::{Compression, Options, Quality, WriteMode, optimize_file};

/// A noisy gradient: far larger than it needs to be, so a lossy run saves a
/// lot, and a different quality gives different bytes.
fn gradient_png() -> Vec<u8> {
    let (w, h) = (96u32, 96u32);
    let mut pixels = Vec::new();
    let mut seed = 12345u32;
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (seed >> 24) % 24;
            pixels.push(((x * 255 / w) + noise).min(255) as u8);
            pixels.push(((y * 255 / h) + noise).min(255) as u8);
            pixels.push((((x + y) * 255 / (w + h)) + noise).min(255) as u8);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// A two-frame animated PNG: a lossy run keeps it lossless, with a note.
fn apng_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 8, 8);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[10; 8 * 8 * 3]).unwrap();
    writer.write_image_data(&[200; 8 * 8 * 3]).unwrap();
    writer.finish().unwrap();
    out
}

const APNG_NOTE: &str = "kept lossless: animated PNG";

fn lossy_at(quality: u8) -> Compression {
    Compression::Lossy(Quality::new(quality).unwrap())
}

/// What the core itself writes for `src` with this compression, from a
/// separate folder: the independent reference for what the binary wrote.
fn core_bytes(src: &Path, compression: Compression) -> Vec<u8> {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("reference");
    let options = Options {
        compression,
        write: WriteMode::NoReplace,
    };
    optimize_file(src, &dest, &options).unwrap();
    fs::read(dest).unwrap()
}

/// Run with a mix of options and paths.
fn run_args(cwd: &Path, args: &[&OsStr]) -> Output {
    let args: Vec<OsString> = args.iter().map(|arg| (*arg).to_owned()).collect();
    lunaepix(cwd, &args.iter().collect::<Vec<_>>())
}

/// The stdout with the milliseconds of every "Done in" line replaced.
fn without_ms(stdout: &str) -> String {
    stdout
        .split('\n')
        .map(|line| {
            if line.starts_with("Done in ") && line.ends_with(" ms!") {
                "Done in <ms> ms!"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of one block, with a note line after its sizes line if given.
fn block_text(block: &[String; 4], note: Option<&str>) -> String {
    let mut lines = vec![block[0].clone(), block[1].clone(), block[2].clone()];
    lines.extend(note.map(str::to_owned));
    lines.push(String::new());
    lines.join("\n") + "\n"
}

#[test]
fn lossy_alone_uses_75() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &gradient_png());

    let output = run_args(tmp.path(), &[OsStr::new("--lossy"), src.as_os_str()]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("Lossy, quality 75\n"), "{stdout:?}");
    // The worker was told 75: the file is what the core writes at 75.
    let written = fs::read(tmp.path().join("optimized-a.png")).unwrap();
    assert_eq!(written, core_bytes(&src, lossy_at(75)));
    assert!(written.len() < fs::metadata(&src).unwrap().len() as usize);
}

#[test]
fn a_given_quality_reaches_the_worker() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &gradient_png());

    let output = run_args(
        tmp.path(),
        &[
            OsStr::new("--lossy"),
            OsStr::new("--quality"),
            OsStr::new("30"),
            src.as_os_str(),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stdout).starts_with("Lossy, quality 30\n"));
    let written = fs::read(tmp.path().join("optimized-a.png")).unwrap();
    assert_eq!(written, core_bytes(&src, lossy_at(30)));
    // And not what 75 would have written.
    assert_ne!(written, core_bytes(&src, lossy_at(75)));
}

#[test]
fn without_lossy_the_output_is_byte_identical_to_todays() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &gradient_png());

    let output = run_on(tmp.path(), &[&src]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    // Nothing before the first block, and nothing but blocks.
    assert!(stdout.starts_with("a.png:\n"), "{stdout:?}");
    assert_eq!(
        blocks(&stdout),
        vec![expected_block(
            "a.png",
            &src,
            &tmp.path().join("optimized-a.png")
        )]
    );
    let written = fs::read(tmp.path().join("optimized-a.png")).unwrap();
    assert_eq!(written, core_bytes(&src, Compression::Lossless));
    assert_ne!(written, core_bytes(&src, lossy_at(75)));
}

#[test]
fn out_creates_a_missing_folder_with_parents_and_writes_original_names() {
    let tmp = TempDir::new().unwrap();
    let png = write(tmp.path(), "a.png", &png_bytes());
    let svg = write(tmp.path(), "b.svg", svg_text().as_bytes());
    let out_dir = tmp.path().join("x").join("y").join("z");

    let output = run_args(
        tmp.path(),
        &[
            OsStr::new("--out"),
            out_dir.as_os_str(),
            png.as_os_str(),
            svg.as_os_str(),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![
            expected_block("a.png", &png, &out_dir.join("a.png")),
            expected_block("b.svg", &svg, &out_dir.join("b.svg")),
        ]
    );
    assert_eq!(listing(&out_dir), names(&["a.png", "b.svg"]));
    // Nothing is written beside the originals, and they are not touched.
    assert_eq!(listing(tmp.path()), names(&["a.png", "b.svg", "x"]));
    assert_eq!(fs::read(&png).unwrap(), png_bytes());
}

#[test]
fn out_never_overwrites_an_existing_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_bytes());
    let out_dir = tmp.path().join("out");
    fs::create_dir(&out_dir).unwrap();
    write(&out_dir, "a.png", b"an earlier result");

    let output = run_args(
        tmp.path(),
        &[OsStr::new("--out"), out_dir.as_os_str(), src.as_os_str()],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    // The earlier file is as it was; the result took the next free name.
    assert_eq!(
        fs::read(out_dir.join("a.png")).unwrap(),
        b"an earlier result"
    );
    assert_eq!(listing(&out_dir), names(&["a-2.png", "a.png"]));
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![expected_block("a.png", &src, &out_dir.join("a-2.png"))]
    );
}

#[test]
fn dir_without_out_writes_to_dir_optimized() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();
    let png = write(&dir, "a.png", &gradient_png());
    let svg = write(&dir, "b.svg", svg_text().as_bytes());

    let output = run_args(
        tmp.path(),
        &[OsStr::new("--lossy"), OsStr::new("--dir"), dir.as_os_str()],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let out_dir = dir.join("optimized");
    let stdout = text(&output.stdout);
    let blocks_text = stdout
        .strip_prefix("Lossy, quality 75\n")
        .expect("mode line");
    assert_eq!(
        blocks(blocks_text),
        vec![
            expected_block("a.png", &png, &out_dir.join("a.png")),
            expected_block("b.svg", &svg, &out_dir.join("b.svg")),
        ]
    );
    assert_eq!(listing(&dir), names(&["a.png", "b.svg", "optimized"]));
    assert_eq!(listing(&out_dir), names(&["a.png", "b.svg"]));
    assert_eq!(
        fs::read(out_dir.join("a.png")).unwrap(),
        core_bytes(&png, lossy_at(75))
    );
}

#[test]
fn dir_with_out_writes_nothing_to_dir_optimized() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();
    let png = write(&dir, "a.png", &png_bytes());
    let svg = write(&dir, "b.svg", svg_text().as_bytes());
    let out_dir = tmp.path().join("elsewhere");

    let output = run_args(
        tmp.path(),
        &[
            OsStr::new("--out"),
            out_dir.as_os_str(),
            OsStr::new("--dir"),
            dir.as_os_str(),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        blocks(&text(&output.stdout)),
        vec![
            expected_block("a.png", &png, &out_dir.join("a.png")),
            expected_block("b.svg", &svg, &out_dir.join("b.svg")),
        ]
    );
    assert_eq!(listing(&out_dir), names(&["a.png", "b.svg"]));
    // The folder of the originals is as it was: no `optimized/` in it.
    assert_eq!(listing(&dir), names(&["a.png", "b.svg"]));
}

#[test]
fn a_lossy_run_prints_the_mode_once_and_notes_after_their_row() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.png", &apng_bytes());
    let svg = write(tmp.path(), "b.svg", svg_text().as_bytes());
    let last = write(tmp.path(), "c.png", &apng_bytes());

    let output = run_args(
        tmp.path(),
        &[
            OsStr::new("--lossy"),
            first.as_os_str(),
            svg.as_os_str(),
            last.as_os_str(),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    // The mode once, then each block; the note under the sizes of the two
    // rows that have one, and nothing under the SVG's (no note for an SVG).
    let expected = format!(
        "Lossy, quality 75\n{}{}{}",
        block_text(
            &expected_block("a.png", &first, &tmp.path().join("optimized-a.png")),
            Some(APNG_NOTE)
        ),
        block_text(
            &expected_block("b.svg", &svg, &tmp.path().join("optimized-b.svg")),
            None
        ),
        block_text(
            &expected_block("c.png", &last, &tmp.path().join("optimized-c.png")),
            Some(APNG_NOTE)
        ),
    );
    assert_eq!(without_ms(&stdout), expected);
    assert_eq!(stdout.matches("Lossy, quality").count(), 1);
}

#[test]
fn a_lossless_run_prints_no_mode_line_and_no_note() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.png", &apng_bytes());
    let svg = write(tmp.path(), "b.svg", svg_text().as_bytes());

    let output = run_on(tmp.path(), &[&first, &svg]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert!(
        !stdout.contains("Lossy") && !stdout.contains(APNG_NOTE),
        "{stdout:?}"
    );
    assert_eq!(
        blocks(&stdout),
        vec![
            expected_block("a.png", &first, &tmp.path().join("optimized-a.png")),
            expected_block("b.svg", &svg, &tmp.path().join("optimized-b.svg")),
        ]
    );
}

#[test]
fn a_lossy_run_reports_a_failed_file_on_stderr_and_exits_1() {
    let tmp = TempDir::new().unwrap();
    let bad = write(tmp.path(), "bad.png", b"this is not a png");
    let good = write(tmp.path(), "a.png", &gradient_png());

    let output = run_args(
        tmp.path(),
        &[OsStr::new("--lossy"), bad.as_os_str(), good.as_os_str()],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("lunaepix: failed (")
            && stderr.ends_with(&format!("): {}\n", bad.display())),
        "{stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    // The mode, then the good file's block only.
    let stdout = text(&output.stdout);
    let blocks_text = stdout
        .strip_prefix("Lossy, quality 75\n")
        .expect("mode line");
    assert_eq!(
        blocks(blocks_text),
        vec![expected_block(
            "a.png",
            &good,
            &tmp.path().join("optimized-a.png")
        )]
    );
}

#[test]
fn a_quality_without_lossy_prints_the_usage_and_exits_2() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", &png_bytes());

    let output = run_with(tmp.path(), &["--quality", "50", "a.png"]);

    assert_usage_error(&output, tmp.path(), &["a.png"]);
    // Both forms are in the usage text.
    assert_eq!(
        text(&output.stderr),
        "usage: lunaepix [--lossy [--quality <0-100>]] [--out <folder>] [--] <file>...\n       lunaepix [--lossy [--quality <0-100>]] [--out <folder>] --dir <dir>\n"
    );
}

#[test]
fn usage_error_with_out_creates_no_out_folder() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", &png_bytes());

    let output = run_with(tmp.path(), &["--out", "new", "--quality", "50", "a.png"]);

    assert_usage_error(&output, tmp.path(), &["a.png"]);
}

#[test]
fn a_dir_that_is_not_a_folder_creates_no_out_folder() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("none");
    let out_dir = tmp.path().join("out");

    let output = run_args(
        tmp.path(),
        &[
            OsStr::new("--out"),
            out_dir.as_os_str(),
            OsStr::new("--dir"),
            missing.as_os_str(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        format!("lunaepix: failed (not a folder): {}\n", missing.display())
    );
    assert_eq!(listing(tmp.path()), names(&[]));
}

#[test]
fn an_out_that_is_a_file_fails_with_exit_1_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_bytes());
    let out_file = write(tmp.path(), "out", b"not a folder");

    let output = run_args(
        tmp.path(),
        &[OsStr::new("--out"), out_file.as_os_str(), src.as_os_str()],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("lunaepix: failed (cannot create the output folder: ")
            && stderr.ends_with(&format!("): {}\n", out_file.display())),
        "{stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    assert_eq!(listing(tmp.path()), names(&["a.png", "out"]));
    assert_eq!(fs::read(&out_file).unwrap(), b"not a folder");
}

#[test]
fn without_out_a_rerun_replaces_the_earlier_output() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &gradient_png());
    let earlier = write(tmp.path(), "optimized-a.png", b"an earlier result");

    let output = run_on(tmp.path(), &[&src]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    // The earlier output now holds the real result, and no numbered name
    // was made beside it.
    assert_eq!(
        fs::read(&earlier).unwrap(),
        core_bytes(&src, Compression::Lossless)
    );
    assert_eq!(listing(tmp.path()), names(&["a.png", "optimized-a.png"]));
}

#[test]
fn without_out_a_dir_rerun_replaces_the_earlier_output_in_optimized() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("pics");
    fs::create_dir(&dir).unwrap();
    let src = write(&dir, "a.png", &gradient_png());
    let out_dir = dir.join("optimized");
    fs::create_dir(&out_dir).unwrap();
    let earlier = write(&out_dir, "a.png", b"an earlier result");

    let output = run_args(tmp.path(), &[OsStr::new("--dir"), dir.as_os_str()]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    assert_eq!(
        fs::read(&earlier).unwrap(),
        core_bytes(&src, Compression::Lossless)
    );
    assert_eq!(listing(&out_dir), names(&["a.png"]));
}
