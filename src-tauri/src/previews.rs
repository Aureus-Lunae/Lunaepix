//! The loaded image and its kept compressed version, served over the
//! `preview` URI scheme, as `thumbs` serves the list's thumbnails.
//!
//! The window asks for `preview://localhost/<key>` (`http://preview.localhost/<key>`
//! on Windows): `<image>` for the copy taken at load, `<image>-<version>`
//! for the kept compressed version. Only those two files of the current
//! image are ever served; anything else is `404` with an empty body. The
//! window process reads the file but never decodes it: the webview does.

use std::fs;
use std::path::Path;

use crate::single::Single;
use crate::thumbs::{ThumbResponse, digits};

/// The answer to one `preview://` request; the same shape as a thumbnail's.
pub type PreviewResponse = ThumbResponse;

/// The most digits of an image key and of a version number.
const KEY_DIGITS: usize = 10;

/// The image key and the version in a request path: exactly
/// `^/(\d{1,10})(?:-(\d{1,10}))?$`, ASCII digits only, on the path as
/// received. A percent-encoded path is not decoded, so it is never a key.
pub fn parse_key(uri_path: &str) -> Option<(u64, Option<u64>)> {
    let key = uri_path.strip_prefix('/')?;
    match key.split_once('-') {
        Some((image, version)) => Some((
            digits(image, KEY_DIGITS)?,
            Some(digits(version, KEY_DIGITS)?),
        )),
        None => Some((digits(key, KEY_DIGITS)?, None)),
    }
}

/// The `Content-Type` of a PNG, JPG, WebP or SVG file by its extension, in
/// any case, as the planner picks the format; `None` for any other.
pub fn content_type_of(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// What the `preview` handler answers to a request with `uri_path` and
/// `query`, both as received. A query (`?x=1`, also an empty `?`) is
/// refused. Otherwise the key is looked up ([`Single::served_file`]): the
/// current image's copy, or its kept version, with the type of its format.
/// Anything else, and a file that cannot be read (a version removed since),
/// is `404`. The file is read without holding `single`'s lock.
pub fn serve(single: &Single, uri_path: &str, query: Option<&str>) -> PreviewResponse {
    if query.is_some() {
        return PreviewResponse::not_found();
    }
    let Some((key, version)) = parse_key(uri_path) else {
        return PreviewResponse::not_found();
    };
    let Some(path) = single.served_file(key, version) else {
        return PreviewResponse::not_found();
    };
    let Some(content_type) = content_type_of(&path) else {
        return PreviewResponse::not_found();
    };
    match fs::read(&path) {
        Ok(body) => PreviewResponse::found(content_type, body),
        Err(_) => PreviewResponse::not_found(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use lunaepix_core::{
        Compression, Crash, FileResult, Outcome, Probe, ProbeAnswer, Quality, RunError,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::events::Event;
    use crate::single::{LoadedImage, PreviewJob};

    fn one_pixel(_: &Path) -> ProbeAnswer {
        ProbeAnswer::Size(Probe {
            width: Some(1),
            height: Some(1),
        })
    }

    fn lossy() -> Compression {
        Compression::Lossy(Quality::new(40).unwrap())
    }

    fn success() -> Option<FileResult> {
        Some(FileResult::Optimized(Outcome {
            before: 8,
            after: 7,
            millis: 1,
            note: None,
        }))
    }

    fn crash() -> Option<FileResult> {
        Some(FileResult::Failed(RunError::Crashed {
            path: PathBuf::new(),
            cause: Crash::NoValidResult,
            stderr: String::new(),
        }))
    }

    fn ok(content_type: &'static str, body: &[u8]) -> PreviewResponse {
        PreviewResponse {
            status: 200,
            content_type: Some(content_type),
            body: body.to_vec(),
        }
    }

    fn not_found() -> PreviewResponse {
        PreviewResponse {
            status: 404,
            content_type: None,
            body: Vec::new(),
        }
    }

    /// A state in a fresh folder, and that folder.
    fn state() -> (TempDir, Single) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        (dir, Single::with_root(root))
    }

    /// Load `name` (holding `original`) into `single`.
    fn load(dir: &TempDir, single: &Single, name: &str, original: &[u8]) {
        let path = dir.path().join(name);
        fs::write(&path, original).unwrap();
        single.load(&path, one_pixel).unwrap();
    }

    /// `job` settles with `result`: the one event it sent, if any.
    fn settle(single: &Single, job: &PreviewJob, result: Option<FileResult>) -> Option<Event> {
        let mut sent = Vec::new();
        single.settle_preview(job, result, &mut |event| sent.push(event));
        assert!(sent.len() <= 1, "{sent:?}");
        sent.pop()
    }

    /// A request whose worker wrote `bytes` and succeeded: its job.
    fn ready(single: &Single, bytes: &[u8]) -> PreviewJob {
        let job = single.begin_preview(lossy(), &mut |_| {}).unwrap();
        fs::write(&job.dest, bytes).unwrap();
        assert!(settle(single, &job, success()).is_some());
        job
    }

    /// Image 1 (`a.png`, "original") with version 1 ready ("version 1").
    fn png_with_version_1() -> (TempDir, Single, PreviewJob) {
        let (dir, single) = state();
        load(&dir, &single, "a.png", b"original");
        let job = ready(&single, b"version 1");
        (dir, single, job)
    }

    // --- parse_key ------------------------------------------------------------

    #[test]
    fn parse_key_accepts_an_image_key_and_a_version_key_up_to_10_digits_each() {
        assert_eq!(parse_key("/7"), Some((7, None)));
        assert_eq!(parse_key("/7-3"), Some((7, Some(3))));
        assert_eq!(parse_key("/0-0"), Some((0, Some(0))));
        assert_eq!(
            parse_key("/9999999999-9999999999"),
            Some((9_999_999_999, Some(9_999_999_999)))
        );
    }

    #[test]
    fn parse_key_rejects_every_other_shape_including_encoded_ones() {
        for bad in [
            "",
            "/",
            "7",
            "7-3",
            "/7-",
            "/-3",
            "/-",
            "/7-3-1",
            "/7/3",
            "/7-3/",
            "//7",
            "/7%2D3",
            "/%37",
            "/7-%33",
            "/12345678901",
            "/7-12345678901",
            "/+7",
            "/7-+3",
            "/-7",
            "/ 7",
            "/7 ",
            "/7?x=1",
            "/7#x",
            "/x",
            "/7-x",
            "/../7",
            "/\u{667}",
            "/7-\u{663}",
        ] {
            assert_eq!(parse_key(bad), None, "{bad:?}");
        }
    }

    // --- content type -------------------------------------------------------------

    #[test]
    fn content_type_of_follows_the_format_in_any_case() {
        for (name, content_type) in [
            ("a.png", "image/png"),
            ("A.PNG", "image/png"),
            ("v3.jpg", "image/jpeg"),
            ("a.JPEG", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.webp", "image/webp"),
            ("a.WebP", "image/webp"),
            ("a.svg", "image/svg+xml"),
            ("a.SVG", "image/svg+xml"),
        ] {
            assert_eq!(
                content_type_of(Path::new(name)),
                Some(content_type),
                "{name}"
            );
        }
    }

    #[test]
    fn content_type_of_is_none_for_other_extensions() {
        for name in ["a.gif", "a.txt", "a", "a.svgz", "a.png.tmp", "png", ".png"] {
            assert_eq!(content_type_of(Path::new(name)), None, "{name}");
        }
    }

    // --- 200 ------------------------------------------------------------------------

    /// `name` loaded and version 1 ready: `/1` and `/1-1` answer 200 with
    /// their bytes and `content_type`.
    fn serves_both(name: &str, content_type: &'static str) {
        let (dir, single) = state();
        load(&dir, &single, name, b"the original");
        ready(&single, b"the version");
        assert_eq!(
            serve(&single, "/1", None),
            ok(content_type, b"the original")
        );
        assert_eq!(
            serve(&single, "/1-1", None),
            ok(content_type, b"the version")
        );
    }

    #[test]
    fn a_png_original_and_its_kept_version_answer_200_image_png() {
        serves_both("a.png", "image/png");
    }

    #[test]
    fn a_jpg_original_and_its_kept_version_answer_200_image_jpeg() {
        serves_both("a.jpg", "image/jpeg");
    }

    #[test]
    fn a_webp_original_and_its_kept_version_answer_200_image_webp() {
        serves_both("a.webp", "image/webp");
    }

    #[test]
    fn an_svg_original_and_its_kept_version_answer_200_image_svg_xml() {
        serves_both("a.svg", "image/svg+xml");
    }

    // --- 404 ------------------------------------------------------------------------

    #[test]
    fn an_older_images_key_answers_404() {
        let (dir, single, _) = png_with_version_1();
        load(&dir, &single, "b.png", b"second");
        assert_eq!(serve(&single, "/1", None), not_found());
        assert_eq!(serve(&single, "/1-1", None), not_found());
        // The image that shows now answers.
        assert_eq!(serve(&single, "/2", None), ok("image/png", b"second"));
    }

    #[test]
    fn a_deleted_version_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        ready(&single, b"version 2");
        assert_eq!(serve(&single, "/1-1", None), not_found());
        assert_eq!(serve(&single, "/1-2", None), ok("image/png", b"version 2"));
    }

    #[test]
    fn a_version_whose_file_was_removed_answers_404() {
        let (_dir, single, job) = png_with_version_1();
        fs::remove_file(&job.dest).unwrap();
        assert_eq!(serve(&single, "/1-1", None), not_found());
    }

    #[test]
    fn a_version_being_made_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        let making = single.begin_preview(lossy(), &mut |_| {}).unwrap();
        fs::write(&making.dest, b"half written").unwrap();
        assert_eq!(serve(&single, "/1-2", None), not_found());
        // The kept one still answers.
        assert_eq!(serve(&single, "/1-1", None), ok("image/png", b"version 1"));
    }

    #[test]
    fn a_failed_version_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        let failing = single.begin_preview(lossy(), &mut |_| {}).unwrap();
        assert!(settle(&single, &failing, crash()).is_some());
        assert_eq!(serve(&single, "/1-2", None), not_found());
        assert_eq!(serve(&single, "/1-1", None), ok("image/png", b"version 1"));
    }

    #[test]
    fn an_encoded_key_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        for path in ["/1%2D1", "/%31", "/%31-1", "/1-%31"] {
            assert_eq!(serve(&single, path, None), not_found(), "{path}");
        }
        assert_eq!(serve(&single, "/1-1", None).status, 200);
    }

    #[test]
    fn a_query_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        assert_eq!(serve(&single, "/1", Some("x=1")), not_found());
        assert_eq!(serve(&single, "/1-1", Some("v=2")), not_found());
        assert_eq!(serve(&single, "/1", None).status, 200);
    }

    #[test]
    fn an_empty_query_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        assert_eq!(serve(&single, "/1", Some("")), not_found());
        assert_eq!(serve(&single, "/1-1", Some("")), not_found());
    }

    #[test]
    fn a_malformed_key_answers_404() {
        let (_dir, single, _) = png_with_version_1();
        for path in ["", "/", "1", "/1-", "/1-1-1", "/x", "/01x", "/1/1", "/../1"] {
            assert_eq!(serve(&single, path, None), not_found(), "{path:?}");
        }
    }

    #[test]
    fn no_image_answers_404() {
        let single = Single::new();
        assert_eq!(serve(&single, "/1", None), not_found());
        assert_eq!(serve(&single, "/1-1", None), not_found());
    }

    /// A state whose image 1 is `copy`, with the extension it has.
    fn holding(copy: PathBuf) -> Single {
        let extension = format!(
            ".{}",
            copy.extension().unwrap_or_default().to_string_lossy()
        );
        Single::with_image(LoadedImage {
            key: 1,
            file_name: "a".into(),
            folder: PathBuf::new(),
            extension,
            permissions: fs::metadata(&copy).unwrap().permissions(),
            copy,
            size: 4,
            width: None,
            height: None,
        })
    }

    #[test]
    fn a_loaded_image_with_another_extension_answers_404() {
        let dir = TempDir::new().unwrap();
        let gif = dir.path().join("original.gif");
        fs::write(&gif, b"GIF8").unwrap();
        assert_eq!(serve(&holding(gif), "/1", None), not_found());
        // The same file under a known extension answers.
        let png = dir.path().join("original.png");
        fs::write(&png, b"GIF8").unwrap();
        assert_eq!(serve(&holding(png), "/1", None), ok("image/png", b"GIF8"));
    }

    // --- fix round 1 ------------------------------------------------------------

    #[test]
    fn a_leading_zero_image_key_serves_the_same_as_without() {
        let (_dir, single, _job) = png_with_version_1();
        assert_eq!(serve(&single, "/01", None), ok("image/png", b"original"));
        assert_eq!(serve(&single, "/01", None), serve(&single, "/1", None));
    }

    #[test]
    fn a_leading_zero_version_serves_the_same_as_without() {
        let (_dir, single, _job) = png_with_version_1();
        assert_eq!(serve(&single, "/1-01", None), ok("image/png", b"version 1"));
        assert_eq!(serve(&single, "/1-01", None), serve(&single, "/1-1", None));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_extension_has_no_content_type() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let name = Path::new(OsStr::from_bytes(b"original.p\xffng"));
        assert_eq!(content_type_of(name), None);
        assert_eq!(
            content_type_of(Path::new("original.png")),
            Some("image/png")
        );
    }

    /// The headers every answer carries, with exactly these values.
    fn has_policy_and_nosniff(response: &PreviewResponse) {
        let http = response.clone().into_http();
        assert_eq!(
            http.headers()["content-security-policy"],
            "default-src 'none'; style-src 'unsafe-inline'"
        );
        assert_eq!(http.headers()["x-content-type-options"], "nosniff");
    }

    #[test]
    fn a_preview_200_carries_the_csp_and_nosniff() {
        let (_dir, single, _job) = png_with_version_1();
        for key in ["/1", "/1-1"] {
            let answer = serve(&single, key, None);
            assert_eq!(answer.status, 200);
            has_policy_and_nosniff(&answer);
        }
    }

    #[test]
    fn a_preview_404_carries_the_csp_and_nosniff() {
        let (_dir, single, _job) = png_with_version_1();
        for (key, query) in [("/2", None), ("/1-2", None), ("/1", Some("x=1"))] {
            let answer = serve(&single, key, query);
            assert_eq!(answer.status, 404);
            has_policy_and_nosniff(&answer);
        }
    }
}
