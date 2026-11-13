//! A chosen output folder: original names, a number on a clash, and writes
//! that never replace anything. Through the public API only: the planner
//! (`plan_files_to`, `plan_folder_to`), `optimize_file` with
//! `WriteMode::NoReplace`, and `images_in_folder`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use lunaepix_core::{
    Compression, Job, OptimizeError, Options, Quality, WriteMode, batch_output_name,
    first_free_name, images_in_folder, optimize_file, plan_files_to, plan_folder_to, save_new,
};
use tempfile::TempDir;

/// A lossless run into a chosen output folder.
const NO_REPLACE: Options = Options {
    compression: Compression::Lossless,
    write: WriteMode::NoReplace,
};

/// Today's run next to the originals.
const REPLACE: Options = Options {
    compression: Compression::Lossless,
    write: WriteMode::Replace,
};

// --- fixtures ---------------------------------------------------------------

/// A small RGBA PNG at the fastest compression, so oxipng has work to do.
/// `seed` gives different pixels, so two sources give different outputs.
fn png(seed: u32) -> Vec<u8> {
    let (w, h) = (24u32, 16u32);
    let pixels: Vec<u8> = (0..w * h * 4)
        .map(|i| ((i * 7 + seed * 31) % 251) as u8)
        .collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// A small baseline JPG.
fn jpg() -> Vec<u8> {
    let (w, h) = (24u32, 16u32);
    let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 241) as u8).collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(&pixels, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// What a lossless run writes for `bytes` named `name`: made with today's
/// write (next to the original, replacing), in a scratch folder of its own,
/// so the expectation does not depend on the code under test.
fn lossless(name: &str, bytes: &[u8]) -> Vec<u8> {
    let scratch = TempDir::new().unwrap();
    let src = write(scratch.path(), name, bytes);
    let dest = scratch.path().join(format!("optimized-{name}"));
    optimize_file(&src, &dest, &REPLACE).unwrap();
    fs::read(dest).unwrap()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn folder(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
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

/// The destinations of `jobs`, which must all be optimised.
fn dests(jobs: &[Job]) -> Vec<PathBuf> {
    jobs.iter()
        .map(|job| match job {
            Job::Optimize { dest, .. } => dest.clone(),
            Job::Skip { src, reason } => panic!("{} skipped: {reason}", src.display()),
        })
        .collect()
}

/// Run every job of `jobs` with `options`; each must succeed.
fn run_all(jobs: &[Job], options: &Options) {
    for job in jobs {
        let Job::Optimize { src, dest } = job else {
            panic!("unexpected {job:?}");
        };
        optimize_file(src, dest, options).unwrap();
    }
}

// --- original names ---------------------------------------------------------

#[test]
fn original_names_land_in_the_output_folder() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let a = write(&originals, "a.png", &png(1));
    let b = write(&originals, "b.jpg", &jpg());

    let jobs = plan_files_to(&[a, b], Some(&out));
    assert_eq!(dests(&jobs), [out.join("a.png"), out.join("b.jpg")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out), ["a.png", "b.jpg"]);
    assert_eq!(
        fs::read(out.join("a.png")).unwrap(),
        lossless("a.png", &png(1))
    );
    assert_eq!(
        fs::read(out.join("b.jpg")).unwrap(),
        lossless("b.jpg", &jpg())
    );
    assert_eq!(listing(&originals), ["a.png", "b.jpg"]);
}

#[test]
fn no_optimized_prefix_in_the_output_folder() {
    // A folder input, with and without going through `plan_files_to`: its
    // files land in the output folder itself, not in `optimized/` or as
    // `optimized-<name>`.
    for through_files in [false, true] {
        let tmp = TempDir::new().unwrap();
        let originals = folder(tmp.path(), "originals");
        let out = folder(tmp.path(), "exports");
        write(&originals, "a.png", &png(1));
        write(&originals, "b.jpg", &jpg());

        let jobs = if through_files {
            plan_files_to(std::slice::from_ref(&originals), Some(&out))
        } else {
            plan_folder_to(&originals, Some(&out))
        };
        assert_eq!(dests(&jobs), [out.join("a.png"), out.join("b.jpg")]);
        run_all(&jobs, &NO_REPLACE);

        assert_eq!(listing(&out), ["a.png", "b.jpg"]);
        assert_eq!(listing(&originals), ["a.png", "b.jpg"]);
    }
}

// --- a clash with an existing file ------------------------------------------

#[test]
fn an_existing_logo_png_makes_logo_1_and_stays_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    write(&out, "logo.png", b"an existing logo");

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(fs::read(out.join("logo.png")).unwrap(), b"an existing logo");
    assert_eq!(
        fs::read(out.join("logo-1.png")).unwrap(),
        lossless("logo.png", &png(1))
    );
    assert_eq!(listing(&out), ["logo-1.png", "logo.png"]);
}

#[test]
fn no_clash_no_number() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    write(&out, "other.png", b"another file");

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out), ["logo.png", "other.png"]);
}

// --- a clash within a run ---------------------------------------------------

#[test]
fn two_inputs_named_a_png_give_a_png_and_a_1_png() {
    let tmp = TempDir::new().unwrap();
    let one = folder(tmp.path(), "one");
    let two = folder(tmp.path(), "two");
    let out = folder(tmp.path(), "exports");
    let first = write(&one, "a.png", &png(1));
    let second = write(&two, "a.png", &png(2));

    let jobs = plan_files_to(&[first, second], Some(&out));
    assert_eq!(dests(&jobs), [out.join("a.png"), out.join("a-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(
        fs::read(out.join("a.png")).unwrap(),
        lossless("a.png", &png(1))
    );
    assert_eq!(
        fs::read(out.join("a-1.png")).unwrap(),
        lossless("a.png", &png(2))
    );
    assert_eq!(listing(&out), ["a-1.png", "a.png"]);
}

#[test]
fn three_inputs_named_a_png_give_a_png_a_1_png_and_a_2_png() {
    let tmp = TempDir::new().unwrap();
    let out = folder(tmp.path(), "exports");
    let inputs: Vec<PathBuf> = ["one", "two", "three"]
        .iter()
        .enumerate()
        .map(|(n, name)| write(&folder(tmp.path(), name), "a.png", &png(n as u32)))
        .collect();

    let jobs = plan_files_to(&inputs, Some(&out));

    assert_eq!(
        dests(&jobs),
        [out.join("a.png"), out.join("a-1.png"), out.join("a-2.png")]
    );
}

#[test]
fn different_names_get_no_number() {
    let tmp = TempDir::new().unwrap();
    let one = folder(tmp.path(), "one");
    let two = folder(tmp.path(), "two");
    let out = folder(tmp.path(), "exports");
    let logo = write(&one, "logo.png", &png(1));
    let mark = write(&two, "mark.png", &png(2));

    let jobs = plan_files_to(&[logo, mark], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo.png"), out.join("mark.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out), ["logo.png", "mark.png"]);
}

// --- letter case ------------------------------------------------------------

#[allow(non_snake_case)]
#[test]
fn an_existing_Logo_PNG_makes_logo_png_become_logo_1() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    write(&out, "Logo.PNG", b"an existing logo");

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(fs::read(out.join("Logo.PNG")).unwrap(), b"an existing logo");
    assert_eq!(listing(&out), ["Logo.PNG", "logo-1.png"]);
}

#[test]
fn logo_png_and_logo_jpg_do_not_clash() {
    // Against an existing file.
    let tmp = TempDir::new().unwrap();
    let one = folder(tmp.path(), "one");
    let two = folder(tmp.path(), "two");
    let out = folder(tmp.path(), "exports");
    let logo_png = write(&one, "logo.png", &png(1));
    write(&out, "logo.jpg", b"an existing jpg");

    let jobs = plan_files_to(std::slice::from_ref(&logo_png), Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo.png")]);

    // Within a run.
    let fresh = folder(tmp.path(), "fresh");
    let logo_jpg = write(&two, "logo.jpg", &jpg());
    let jobs = plan_files_to(&[logo_png, logo_jpg], Some(&fresh));
    assert_eq!(
        dests(&jobs),
        [fresh.join("logo.png"), fresh.join("logo.jpg")]
    );
    run_all(&jobs, &NO_REPLACE);
    assert_eq!(listing(&fresh), ["logo.jpg", "logo.png"]);
}

// --- names beyond ASCII (Review Focus 4) --------------------------------------

#[test]
fn non_ascii_names_clash_case_insensitively_and_keep_their_bytes() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "Mobilis Logo – zwart é.png", &png(1));
    write(&out, "MOBILIS LOGO – ZWART É.PNG", b"an existing logo");

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("Mobilis Logo – zwart é-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(
        fs::read(out.join("MOBILIS LOGO – ZWART É.PNG")).unwrap(),
        b"an existing logo"
    );
    assert_eq!(
        listing(&out),
        ["MOBILIS LOGO – ZWART É.PNG", "Mobilis Logo – zwart é-1.png"]
    );

    // Within a run: `Ä.png` and `ä.png` clash too.
    let one = folder(tmp.path(), "one");
    let two = folder(tmp.path(), "two");
    let fresh = folder(tmp.path(), "fresh");
    let upper = write(&one, "Ä.png", &png(1));
    let lower = write(&two, "ä.png", &png(2));
    let jobs = plan_files_to(&[upper, lower], Some(&fresh));
    assert_eq!(dests(&jobs), [fresh.join("Ä.png"), fresh.join("ä-1.png")]);
}

#[test]
fn non_ascii_names_that_differ_beyond_case_do_not_clash() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "Mobilis Logo – zwart é.png", &png(1));
    write(&out, "MOBILIS LOGO – ZWART E.PNG", b"no accent");
    write(&out, "Mobilis Logo - zwart é.png", b"a hyphen, not a dash");

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("Mobilis Logo – zwart é.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out).len(), 3);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_name_gets_its_number_with_its_bytes_intact() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    // Latin-1 é: not valid UTF-8. The existing name differs in letter case.
    let src = originals.join(OsString::from_vec(b"caf\xE9.png".to_vec()));
    fs::write(&src, png(1)).unwrap();
    let existing = out.join(OsString::from_vec(b"CAF\xE9.PNG".to_vec()));
    fs::write(&existing, b"an existing file").unwrap();

    let jobs = plan_files_to(&[src], Some(&out));
    let dest = dests(&jobs).remove(0);
    assert_eq!(dest.parent().unwrap(), out);
    assert_eq!(dest.file_name().unwrap().as_bytes(), b"caf\xE9-1.png");
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(fs::read(&existing).unwrap(), b"an existing file");
    assert_eq!(fs::read(&dest).unwrap(), lossless("a.png", &png(1)));
    assert_eq!(listing(&out).len(), 2);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_name_without_a_clash_keeps_its_exact_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = originals.join(OsString::from_vec(b"caf\xE9.png".to_vec()));
    fs::write(&src, png(1)).unwrap();

    let jobs = plan_files_to(&[src], Some(&out));
    run_all(&jobs, &NO_REPLACE);

    let names = listing(&out);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].as_bytes(), b"caf\xE9.png");
}

// --- two sources that differ only in letter case (Review Focus 3) -------------

/// `x/a.png` and `y/A.png` planned and run into one output folder, then
/// checked: both outputs are there, each with the bytes of its own source's
/// optimised version, so neither replaced the other. The planner numbers the
/// second name (`A-1.png`) because it compares names case-insensitively on
/// every platform, whatever the folder's file system does.
fn two_case_variants_are_both_kept() {
    let tmp = TempDir::new().unwrap();
    let x = folder(tmp.path(), "x");
    let y = folder(tmp.path(), "y");
    let out = folder(tmp.path(), "exports");
    let lower = write(&x, "a.png", &png(1));
    let upper = write(&y, "A.png", &png(2));

    let jobs = plan_files_to(&[lower, upper], Some(&out));
    assert_eq!(dests(&jobs), [out.join("a.png"), out.join("A-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    // Two entries: on a folder that ignores case, a replaced file would leave one.
    assert_eq!(listing(&out), ["A-1.png", "a.png"]);
    assert_eq!(
        fs::read(out.join("a.png")).unwrap(),
        lossless("a.png", &png(1))
    );
    assert_eq!(
        fs::read(out.join("A-1.png")).unwrap(),
        lossless("A.png", &png(2))
    );
}

/// The file systems of Windows and the Mac ignore case: `a.png` and `A.png`
/// would be one file there.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn two_sources_whose_names_differ_only_in_case_both_reach_one_output_folder() {
    two_case_variants_are_both_kept();
}

/// The same planner on Linux, where the folder could hold `a.png` and
/// `A.png` side by side: the second is numbered all the same, so the plan is
/// the one the Windows and Mac test checks.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_names_that_differ_only_in_case_are_numbered_all_the_same() {
    two_case_variants_are_both_kept();
}

// --- a file that appears while the run goes ----------------------------------

#[test]
fn a_file_appearing_before_the_write_is_not_overwritten_and_the_next_number_is_used() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    let jobs = plan_files_to(std::slice::from_ref(&src), Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo.png")]);

    // Planned, not yet written: now the name is taken.
    write(&out, "logo.png", b"appeared after planning");
    let outcome = optimize_file(&src, &out.join("logo.png"), &NO_REPLACE).unwrap();

    assert_eq!(
        fs::read(out.join("logo.png")).unwrap(),
        b"appeared after planning"
    );
    let written = fs::read(out.join("logo-1.png")).unwrap();
    assert_eq!(written, lossless("logo.png", &png(1)));
    assert_eq!(outcome.after, written.len() as u64);
    assert_eq!(listing(&out), ["logo-1.png", "logo.png"]);
}

#[test]
fn with_nothing_appearing_the_planned_name_is_used() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));

    let jobs = plan_files_to(&[src], Some(&out));
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out), ["logo.png"]);
    assert_eq!(
        fs::read(out.join("logo.png")).unwrap(),
        lossless("logo.png", &png(1))
    );
}

// --- the originals' own folder -----------------------------------------------

#[test]
fn the_originals_own_folder_writes_photo_1_and_leaves_photo_unchanged() {
    for through_folder in [false, true] {
        let tmp = TempDir::new().unwrap();
        let src = write(tmp.path(), "photo.jpg", &jpg());

        let jobs = if through_folder {
            plan_folder_to(tmp.path(), Some(tmp.path()))
        } else {
            plan_files_to(std::slice::from_ref(&src), Some(tmp.path()))
        };
        assert_eq!(dests(&jobs), [tmp.path().join("photo-1.jpg")]);
        run_all(&jobs, &NO_REPLACE);

        assert_eq!(fs::read(&src).unwrap(), jpg());
        assert_eq!(
            fs::read(tmp.path().join("photo-1.jpg")).unwrap(),
            lossless("photo.jpg", &jpg())
        );
        assert_eq!(listing(tmp.path()), ["photo-1.jpg", "photo.jpg"]);
    }
}

#[test]
fn another_folder_writes_photo_without_number() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "photo.jpg", &jpg());

    let jobs = plan_files_to(&[src], Some(&out));
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(listing(&out), ["photo.jpg"]);
    assert_eq!(listing(&originals), ["photo.jpg"]);
}

// --- the numbering bound ------------------------------------------------------

/// An output folder holding `logo.png` and `logo-1.png` … `logo-<last>.png`.
fn taken_up_to(out: &Path, last: u32) {
    fs::write(out.join("logo.png"), b"").unwrap();
    for n in 1..=last {
        fs::write(out.join(format!("logo-{n}.png")), b"").unwrap();
    }
}

#[test]
fn an_output_folder_with_every_number_taken_fails_the_file_with_the_reason() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    taken_up_to(&out, 9999);

    let jobs = plan_files_to(std::slice::from_ref(&src), Some(&out));
    let dest = dests(&jobs).remove(0);
    let error = optimize_file(&src, &dest, &NO_REPLACE).unwrap_err();

    match &error {
        OptimizeError::Io { path, .. } => assert_eq!(path, &dest),
        other => panic!("{other:?}"),
    }
    assert_eq!(error.reason(), "no free name in the output folder");
    let names = listing(&out);
    assert_eq!(names.len(), 10000);
    assert!(
        names
            .iter()
            .all(|n| fs::read(out.join(n)).unwrap().is_empty()),
        "an existing file was written to"
    );
    assert!(!out.join("logo-10000.png").exists());
}

#[test]
fn the_last_number_is_used_when_only_it_is_free() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    taken_up_to(&out, 9998);

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo-9999.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(
        fs::read(out.join("logo-9999.png")).unwrap(),
        lossless("logo.png", &png(1))
    );
}

#[test]
fn minus_one_is_used_when_every_higher_number_is_taken() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let out = folder(tmp.path(), "exports");
    let src = write(&originals, "logo.png", &png(1));
    fs::write(out.join("logo.png"), b"").unwrap();
    for n in 2..=9999 {
        fs::write(out.join(format!("logo-{n}.png")), b"").unwrap();
    }

    let jobs = plan_files_to(&[src], Some(&out));
    assert_eq!(dests(&jobs), [out.join("logo-1.png")]);
    run_all(&jobs, &NO_REPLACE);

    assert_eq!(
        fs::read(out.join("logo-1.png")).unwrap(),
        lossless("logo.png", &png(1))
    );
    assert!(!out.join("logo-0.png").exists());
}

// --- next to the originals ------------------------------------------------------

#[test]
fn without_an_output_folder_optimized_name_replaces_as_today() {
    // A loose file: `optimized-<name>` beside it, replacing an earlier one.
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png(1));
    write(tmp.path(), "optimized-a.png", b"an earlier output");

    let jobs = plan_files_to(std::slice::from_ref(&src), None);
    assert_eq!(dests(&jobs), [tmp.path().join("optimized-a.png")]);
    run_all(&jobs, &REPLACE);

    assert_eq!(
        fs::read(tmp.path().join("optimized-a.png")).unwrap(),
        lossless("a.png", &png(1))
    );

    // A folder: its `optimized/` subfolder, replacing an earlier output.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "b.png", &png(2));
    let earlier = folder(dir.path(), "optimized");
    write(&earlier, "b.png", b"an earlier output");

    let jobs = plan_folder_to(dir.path(), None);
    assert_eq!(dests(&jobs), [earlier.join("b.png")]);
    run_all(&jobs, &REPLACE);

    assert_eq!(
        fs::read(earlier.join("b.png")).unwrap(),
        lossless("b.png", &png(2))
    );
}

#[test]
fn nothing_is_written_outside_the_originals_folder() {
    let tmp = TempDir::new().unwrap();
    let originals = folder(tmp.path(), "originals");
    let exports = folder(tmp.path(), "exports");
    let src = write(&originals, "a.png", &png(1));
    write(&originals, "optimized-a.png", b"an earlier output");

    let jobs = plan_files_to(&[src], None);
    run_all(&jobs, &REPLACE);

    assert_eq!(listing(tmp.path()), ["exports", "originals"]);
    assert_eq!(listing(&exports), Vec::<OsString>::new());
    assert_eq!(listing(&originals), ["a.png", "optimized-a.png"]);
}

// --- where a no-replace run may write ----------------------------------------------

#[test]
fn no_replace_mode_accepts_any_folder() {
    let lossy = Compression::Lossy(Quality::new(75).unwrap());
    for compression in [Compression::Lossless, lossy] {
        let tmp = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let src = write(tmp.path(), "a.png", &png(1));
        let options = Options {
            compression,
            write: WriteMode::NoReplace,
        };

        for dest in [
            tmp.path().join("elsewhere").join("deeper").join("a.png"),
            elsewhere.path().join("a.png"),
        ] {
            let outcome = optimize_file(&src, &dest, &options).unwrap();
            assert_eq!(
                fs::metadata(&dest).unwrap().len(),
                outcome.after,
                "{compression:?}"
            );
        }
        assert_eq!(fs::read(&src).unwrap(), png(1));
    }
}

#[test]
fn a_no_replace_write_that_fails_names_the_destination_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png(1));
    write(tmp.path(), "blocker", b"a file, not a folder");
    let dest = tmp.path().join("blocker").join("a.png");

    let error = optimize_file(&src, &dest, &NO_REPLACE).unwrap_err();

    match &error {
        OptimizeError::Io { path, .. } => assert_eq!(path, &dest),
        other => panic!("{other:?}"),
    }
    assert_eq!(listing(tmp.path()), ["a.png", "blocker"]);
    assert_eq!(
        fs::read(tmp.path().join("blocker")).unwrap(),
        b"a file, not a folder"
    );
}

// --- the images in a dropped folder -------------------------------------------------

#[test]
fn images_in_folder_lists_supported_files_only_sorted_and_skips_subfolders() {
    let tmp = TempDir::new().unwrap();
    for name in [
        "b.png",
        "optimized-a.jpg",
        "a.jpg",
        "d.webp",
        "c.SVG",
        "notes.txt",
    ] {
        write(tmp.path(), name, b"x");
    }
    let sub = folder(tmp.path(), "sub");
    write(&sub, "deep.png", b"x");
    let optimized = folder(tmp.path(), "optimized");
    write(&optimized, "x.png", b"x");
    folder(tmp.path(), "folder.png");

    let images = images_in_folder(tmp.path()).unwrap();

    let expected: Vec<PathBuf> = ["a.jpg", "b.png", "c.SVG", "d.webp", "optimized-a.jpg"]
        .iter()
        .map(|name| tmp.path().join(name))
        .collect();
    assert_eq!(images, expected);
}

#[test]
fn a_folder_with_no_images_gives_an_empty_list() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "notes.txt", b"x");
    write(tmp.path(), "a.png.txt", b"x");
    let sub = folder(tmp.path(), "sub");
    write(&sub, "deep.png", b"x");

    assert_eq!(images_in_folder(tmp.path()).unwrap(), Vec::<PathBuf>::new());
}

#[test]
fn images_in_folder_fails_for_a_missing_folder() {
    let tmp = TempDir::new().unwrap();
    let file = write(tmp.path(), "a.png", b"x");

    assert!(images_in_folder(&tmp.path().join("missing")).is_err());
    assert!(images_in_folder(&file).is_err());
}

// --- save_new: exactly this name, never a number -----------------------------

/// Some permissions to hand to `save_new`: a file's, as the real Save hands
/// in (the original image's). A folder's would carry the directory attribute
/// on Windows, which a new file refuses ("The parameter is incorrect"). The
/// file sits in a folder of its own, so no listing in a test sees it.
fn some_permissions() -> fs::Permissions {
    let donor_folder = TempDir::new().unwrap();
    let donor = write(donor_folder.path(), "donor.png", b"");
    fs::metadata(donor).unwrap().permissions()
}

#[test]
fn save_new_writes_exactly_the_bytes_under_exactly_the_name() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("vakantie-café.png");

    save_new(&dest, b"new bytes", some_permissions()).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), b"new bytes");
    assert_eq!(listing(tmp.path()), ["vakantie-café.png"]);
}

#[test]
fn save_new_refuses_a_taken_name_and_leaves_that_file_byte_for_byte() {
    let tmp = TempDir::new().unwrap();
    let dest = write(tmp.path(), "logo.png", b"existing");

    let error = save_new(&dest, b"new", some_permissions()).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::AlreadyExists, "{error}");
    assert_eq!(fs::read(&dest).unwrap(), b"existing");
    assert_eq!(listing(tmp.path()), ["logo.png"]);
}

#[test]
fn save_new_refuses_a_taken_name_that_is_a_folder_and_leaves_it_alone() {
    let tmp = TempDir::new().unwrap();
    let dest = folder(tmp.path(), "logo.png");
    write(&dest, "inside", b"kept");

    let error = save_new(&dest, b"new", some_permissions()).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::AlreadyExists, "{error}");
    assert!(dest.is_dir());
    assert_eq!(fs::read(dest.join("inside")).unwrap(), b"kept");
    assert_eq!(listing(tmp.path()), ["logo.png"]);
}

#[test]
fn save_new_never_numbers() {
    // `write_new` would take `logo-1.png` here; a typed name is not changed.
    let tmp = TempDir::new().unwrap();
    let dest = write(tmp.path(), "logo.png", b"existing");

    let error = save_new(&dest, b"new", some_permissions()).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::AlreadyExists, "{error}");
    assert_eq!(listing(tmp.path()), ["logo.png"]);
}

#[test]
fn save_new_refuses_a_missing_folder_and_creates_nothing() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("exports").join("logo.png");

    let error = save_new(&dest, b"new", some_permissions()).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
    assert_eq!(listing(tmp.path()), Vec::<OsString>::new());
}

#[test]
fn save_new_leaves_no_temporary_file_after_a_refusal() {
    let tmp = TempDir::new().unwrap();
    let dest = write(tmp.path(), "logo.png", b"existing");
    let before = listing(tmp.path());

    let error = save_new(&dest, b"new", some_permissions()).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::AlreadyExists, "{error}");
    assert_eq!(listing(tmp.path()), before);
}

// --- first_free_name -----------------------------------------------------------

#[test]
fn first_free_name_gives_the_name_when_free() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "other.png", b"x");

    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a.png"))
    );
}

#[test]
fn first_free_name_gives_minus_one_then_minus_two() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", b"x");
    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a-1.png"))
    );

    write(tmp.path(), "a-1.png", b"x");
    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a-2.png"))
    );
}

#[test]
fn first_free_name_skips_a_taken_minus_one() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", b"x");
    write(tmp.path(), "a-1.png", b"x");

    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a-2.png"))
    );
}

#[test]
fn first_free_name_does_not_number_while_the_name_is_free() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a-1.png", b"x");

    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a.png"))
    );
}

#[test]
fn first_free_name_treats_a_name_that_differs_only_in_case_as_taken() {
    // The planner compares names this way on every platform.
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "A.PNG", b"x");

    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a-1.png"))
    );
}

#[test]
fn first_free_name_counts_a_folder_as_taken() {
    let tmp = TempDir::new().unwrap();
    folder(tmp.path(), "a.png");

    assert_eq!(
        first_free_name(tmp.path(), OsStr::new("a.png")),
        Some(OsString::from("a-1.png"))
    );
}

#[test]
fn first_free_name_in_a_missing_folder_gives_the_name() {
    let tmp = TempDir::new().unwrap();

    assert_eq!(
        first_free_name(&tmp.path().join("missing"), OsStr::new("a.png")),
        Some(OsString::from("a.png"))
    );
}

#[cfg(unix)]
#[test]
fn first_free_name_keeps_the_bytes_of_a_name_that_is_not_utf8() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let tmp = TempDir::new().unwrap();
    let name = OsString::from_vec(b"caf\xe9.png".to_vec());
    fs::write(tmp.path().join(&name), b"x").unwrap();

    let free = first_free_name(tmp.path(), &name).unwrap();

    assert_eq!(free.as_bytes(), b"caf\xe9-1.png");
    assert_ne!(free, name);
}

// --- batch_output_name ---------------------------------------------------------

#[test]
fn batch_output_name_prefixes_only_next_to_the_original() {
    let src = Path::new("/photos/holiday.jpg");

    assert_eq!(
        batch_output_name(src, false),
        OsString::from("optimized-holiday.jpg")
    );
    assert_eq!(batch_output_name(src, true), OsString::from("holiday.jpg"));
}

#[test]
fn batch_output_name_is_the_name_the_planner_gives() {
    let tmp = TempDir::new().unwrap();
    let out = folder(tmp.path(), "exports");
    let src = write(tmp.path(), "holiday.jpg", &jpg());

    let next_to = dests(&plan_files_to(std::slice::from_ref(&src), None));
    let chosen = dests(&plan_files_to(std::slice::from_ref(&src), Some(&out)));

    assert_eq!(
        next_to[0].file_name().unwrap(),
        batch_output_name(&src, false)
    );
    assert_eq!(
        chosen[0].file_name().unwrap(),
        batch_output_name(&src, true)
    );
    assert_ne!(
        batch_output_name(&src, false),
        batch_output_name(&src, true)
    );
}

#[cfg(unix)]
#[test]
fn batch_output_name_keeps_the_bytes_of_a_name_that_is_not_utf8() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let src = PathBuf::from(OsString::from_vec(b"/photos/caf\xe9.jpg".to_vec()));

    assert_eq!(
        batch_output_name(&src, false).as_bytes(),
        b"optimized-caf\xe9.jpg"
    );
    assert_eq!(batch_output_name(&src, true).as_bytes(), b"caf\xe9.jpg");
}
