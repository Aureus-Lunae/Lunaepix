//! Thumbnails for the window's list, served over the `thumb` URI scheme.
//!
//! Every `list-changed` gives each image row a key, `<list>-<index>`
//! ([`Thumbs::snapshot`]). The window asks for `thumb://localhost/<key>`
//! (`http://thumb.localhost/<key>` on Windows), and [`Thumbs::answer_request`]
//! finds the row's file through the key alone: an address with a query is
//! refused, and nothing else in it is read. A worker process makes the thumbnail ([`serve`] with
//! [`lunaepix_core::run_thumb`]), at most [`MAX_THUMB_WORKERS`] at a time,
//! and the answers of the current list are kept until a new list starts,
//! up to [`MAX_THUMB_CACHE_BYTES`] of them.
//!
//! A key never changes its file: the list number is raised when a new list
//! starts ([`Thumbs::new_list`]) and whenever a snapshot would give a key
//! that was sent another file. So the webview may keep a key's picture for
//! as long as it likes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Condvar, Mutex, PoisonError};

use lunaepix_core::{ThumbAnswer, run_thumb};
use tauri::http::header::{
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderValue, X_CONTENT_TYPE_OPTIONS,
};
use tauri::http::{Request, Response, StatusCode};

/// The policy of every `thumb` and `preview` answer: nothing loads or runs
/// from it (no scripts, no fetches, no frames); inline styles stay, so an SVG
/// opened by itself still draws as it should.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'";

use crate::session::lock;

/// At most this many thumbnail workers run at a time; other requests wait.
pub const MAX_THUMB_WORKERS: usize = 2;

/// The most bytes of thumbnails kept per list. An answer that would pass it
/// is still served, but made again when it is asked for again.
pub const MAX_THUMB_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// The number of the first list.
const FIRST_LIST: u64 = 1;

/// The most digits of a key's list number and of its row index.
const LIST_DIGITS: usize = 10;
const INDEX_DIGITS: usize = 4;

/// The list number and the row index in a request path: exactly
/// `^/(\d{1,10})-(\d{1,4})$`, ASCII digits only, on the path as received. A
/// percent-encoded path is not decoded, so it is never a key.
pub fn parse_key(uri_path: &str) -> Option<(u64, usize)> {
    let (list, index) = uri_path.strip_prefix('/')?.split_once('-')?;
    Some((digits(list, LIST_DIGITS)?, digits(index, INDEX_DIGITS)?))
}

/// `text` as a number, if it is 1 to `most` ASCII digits and nothing else.
pub(crate) fn digits<T: FromStr>(text: &str, most: usize) -> Option<T> {
    let all_digits = (1..=most).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
    // At most 10 digits always fit a `u64`, and 4 a `usize`.
    all_digits.then(|| text.parse().ok()).flatten()
}

/// The answer to one `thumb://` request: `200 image/png`, `200
/// image/svg+xml`, or `404` with no type and an empty body. The `preview`
/// scheme answers in the same shape (`previews::PreviewResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbResponse {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub body: Vec<u8>,
}

impl ThumbResponse {
    /// The answer to anything that is not a thumbnail of the current list.
    pub fn not_found() -> Self {
        ThumbResponse {
            status: 404,
            content_type: None,
            body: Vec::new(),
        }
    }

    /// `200` with `body` of `content_type`.
    pub(crate) fn found(content_type: &'static str, body: Vec<u8>) -> Self {
        ThumbResponse {
            status: 200,
            content_type: Some(content_type),
            body,
        }
    }

    fn png(body: Vec<u8>) -> Self {
        Self::found("image/png", body)
    }

    fn svg(body: Vec<u8>) -> Self {
        Self::found("image/svg+xml", body)
    }

    /// This answer as the webview's response: the status, a `Content-Type`
    /// only when there is a type, the two headers every answer carries
    /// ([`POLICY`] and `nosniff`), and the body. A status the HTTP types
    /// refuse (none of ours) gives an empty `404` with those two headers.
    pub fn into_http(self) -> Response<Vec<u8>> {
        let mut builder = Response::builder()
            .status(self.status)
            .header(CONTENT_SECURITY_POLICY, POLICY)
            .header(X_CONTENT_TYPE_OPTIONS, "nosniff");
        if let Some(content_type) = self.content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        builder.body(self.body).unwrap_or_else(|_| {
            let mut response = Response::new(Vec::new());
            *response.status_mut() = StatusCode::NOT_FOUND;
            let headers = response.headers_mut();
            headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(POLICY));
            headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            response
        })
    }
}

/// The thumbnail keys of the list, the answers kept for them, and the pool
/// of thumbnail workers.
#[derive(Debug)]
pub struct Thumbs {
    list: Mutex<List>,
    pool: Pool,
    /// The most bytes of answer bodies kept per list.
    budget: usize,
}

/// The current list, as its keys see it.
#[derive(Debug)]
struct List {
    /// Raised by one for every new list.
    number: u64,
    /// The file of each row of the last snapshot; `None` for a row that has
    /// no thumbnail.
    rows: Vec<Option<PathBuf>>,
    /// The file each index has named under this list number, also while its
    /// row is skipped, so that a key once sent keeps its file.
    named: Vec<Option<PathBuf>>,
    /// The answers kept, by row index: thumbnails, and "cannot be shown".
    answers: HashMap<usize, ThumbResponse>,
    /// The bytes of the bodies in `answers`.
    kept_bytes: usize,
}

impl List {
    /// Start the next list: no rows and no answers yet.
    fn start_next(&mut self) {
        self.number += 1;
        self.rows.clear();
        self.named.clear();
        self.answers.clear();
        self.kept_bytes = 0;
    }
}

/// What the list says about a key.
enum Found {
    /// The answer is known: a kept one, or `404`.
    Answer(ThumbResponse),
    /// The thumbnail of this file is still to be made.
    Make(PathBuf),
}

impl Default for Thumbs {
    fn default() -> Self {
        Self::with_budget(MAX_THUMB_CACHE_BYTES)
    }
}

impl Thumbs {
    /// With the budget [`MAX_THUMB_CACHE_BYTES`].
    pub fn new() -> Self {
        Self::default()
    }

    /// With a budget of `bytes` of answer bodies kept per list; the tests
    /// use a small one.
    pub fn with_budget(bytes: usize) -> Self {
        Thumbs {
            list: Mutex::new(List {
                number: FIRST_LIST,
                rows: Vec::new(),
                named: Vec::new(),
                answers: HashMap::new(),
                kept_bytes: 0,
            }),
            pool: Pool::default(),
            budget: bytes,
        }
    }

    /// Take the rows of a `list-changed`: the image path of each row, or
    /// `None` for a skipped row and the over-limit row. Returns each row's
    /// key, `<list>-<index>`, or `None`. When a row index that has named a
    /// file under this list number would get another file (a row dropped
    /// from the middle of the list), the list number is raised first, so no
    /// key that was sent ever names another file.
    pub fn snapshot(&self, rows: &[Option<PathBuf>]) -> Vec<Option<String>> {
        let mut list = lock(&self.list);
        let renamed = rows
            .iter()
            .zip(&list.named)
            .any(|pair| matches!(pair, (Some(row), Some(named)) if row != named));
        if renamed {
            list.start_next();
        }
        let len = list.named.len().max(rows.len());
        list.named.resize(len, None);
        for (named, row) in list.named.iter_mut().zip(rows) {
            if row.is_some() {
                named.clone_from(row);
            }
        }
        list.rows = rows.to_vec();
        let number = list.number;
        rows.iter()
            .enumerate()
            .map(|(index, row)| row.as_ref().map(|_| format!("{number}-{index}")))
            .collect()
    }

    /// A new list starts (Clear, or the first add after a finished run):
    /// the list number is raised, and the rows and the answers kept are
    /// dropped. A thumbnail still being made for the old list is not kept.
    pub fn new_list(&self) {
        lock(&self.list).start_next();
    }

    /// The answer to a request for `uri_path`. Only a key of the current
    /// list for a row with a file is looked up; its thumbnail is made by
    /// `make` (on this thread) unless an answer is kept. At most
    /// [`MAX_THUMB_WORKERS`] calls of `make` run at a time; a request waits
    /// for its turn, then looks again, as the list may have ended or another
    /// request made the same key meanwhile.
    pub fn answer(&self, uri_path: &str, make: impl Fn(&Path) -> ThumbAnswer) -> ThumbResponse {
        let Some(key) = parse_key(uri_path) else {
            return ThumbResponse::not_found();
        };
        if let Found::Answer(response) = self.find(key) {
            return response;
        }
        // Held until the answer is stored, so that a request waiting for
        // the same key finds it.
        let _slot = self.pool.take();
        let path = match self.find(key) {
            Found::Answer(response) => return response,
            Found::Make(path) => path,
        };
        let made = make(&path);
        self.store(key, &path, made)
    }

    /// The answer to a `thumb` request. A request with a query (`?x=1`, also
    /// an empty `?`) is refused: `404`, and nothing is made. Otherwise the
    /// path, as received, is looked up as by [`Thumbs::answer`]. Both
    /// platform addresses (`thumb://localhost/…` and
    /// `http://thumb.localhost/…`) have the same path. A fragment never
    /// reaches here: the `http` crate's `Uri` drops it when it parses.
    pub fn answer_request(
        &self,
        request: &Request<Vec<u8>>,
        make: impl Fn(&Path) -> ThumbAnswer,
    ) -> ThumbResponse {
        let uri = request.uri();
        if uri.query().is_some() {
            return ThumbResponse::not_found();
        }
        self.answer(uri.path(), make)
    }

    /// What the current list says about the key `(number, index)`.
    fn find(&self, (number, index): (u64, usize)) -> Found {
        let list = lock(&self.list);
        let path = match list.rows.get(index) {
            Some(Some(path)) if list.number == number => path,
            _ => return Found::Answer(ThumbResponse::not_found()),
        };
        match list.answers.get(&index) {
            Some(response) => Found::Answer(response.clone()),
            None => Found::Make(path.clone()),
        }
    }

    /// The response to what `make` answered for `path`, the file of the key
    /// `(number, index)`; kept when it is a thumbnail or "cannot be shown",
    /// the key's list is still the current one, and its body fits in what
    /// is left of the budget ("cannot be shown" has no body, so it always
    /// fits). An SVG answer for a file whose name does not end in `.svg` is
    /// not believed.
    fn store(
        &self,
        (number, index): (u64, usize),
        path: &Path,
        made: ThumbAnswer,
    ) -> ThumbResponse {
        let (response, keep) = match made {
            ThumbAnswer::Png(bytes) => (ThumbResponse::png(bytes), true),
            ThumbAnswer::Svg(bytes) if is_svg_name(path) => (ThumbResponse::svg(bytes), true),
            ThumbAnswer::CannotShow => (ThumbResponse::not_found(), true),
            ThumbAnswer::Svg(_) | ThumbAnswer::Failed => (ThumbResponse::not_found(), false),
        };
        let mut list = lock(&self.list);
        if list.number != number {
            return ThumbResponse::not_found();
        }
        let kept_bytes = list.kept_bytes + response.body.len();
        if keep && kept_bytes <= self.budget {
            list.kept_bytes = kept_bytes;
            list.answers.insert(index, response.clone());
        }
        response
    }

    /// How many requests wait for a worker slot now.
    #[cfg(test)]
    fn waiting(&self) -> usize {
        lock(&self.pool.state).waiting
    }
}

/// Whether the file name of `path` ends in `.svg`, in any case.
fn is_svg_name(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.as_encoded_bytes()
            .to_ascii_lowercase()
            .ends_with(b".svg")
    })
}

/// The slots of the thumbnail workers.
#[derive(Debug, Default)]
struct Pool {
    state: Mutex<PoolState>,
    freed: Condvar,
}

#[derive(Debug, Default)]
struct PoolState {
    /// Slots taken.
    busy: usize,
    /// Requests waiting for a slot.
    waiting: usize,
}

/// A worker slot, given back when dropped (also when `make` panics).
struct Slot<'a>(&'a Pool);

impl Pool {
    /// A slot, once fewer than [`MAX_THUMB_WORKERS`] are taken.
    fn take(&self) -> Slot<'_> {
        let mut state = lock(&self.state);
        state.waiting += 1;
        let mut state = self
            .freed
            .wait_while(state, |state| state.busy >= MAX_THUMB_WORKERS)
            .unwrap_or_else(PoisonError::into_inner);
        state.waiting -= 1;
        state.busy += 1;
        Slot(self)
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).busy -= 1;
        self.0.freed.notify_one();
    }
}

/// What the `thumb` handler answers to `request`: [`Thumbs::answer_request`],
/// with each thumbnail made by `program` in a worker process that this
/// thread starts and waits for ([`run_thumb`]).
pub fn serve(thumbs: &Thumbs, program: &Path, request: &Request<Vec<u8>>) -> ThumbResponse {
    thumbs.answer_request(request, |src| run_thumb(program, src))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use tauri::http::header::CONTENT_TYPE;

    use super::*;

    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// How long a test waits for another thread before it fails.
    const WAIT: Duration = Duration::from_secs(10);

    fn path(name: &str) -> PathBuf {
        Path::new("/pictures").join(name)
    }

    /// A PNG answer that names the file it was made from, so a response
    /// shows which row's file was made.
    fn png_of(path: &Path) -> Vec<u8> {
        [PNG_SIGNATURE, path.as_os_str().as_encoded_bytes()].concat()
    }

    fn png_response(body: Vec<u8>) -> ThumbResponse {
        ThumbResponse {
            status: 200,
            content_type: Some("image/png"),
            body,
        }
    }

    fn svg_response(body: Vec<u8>) -> ThumbResponse {
        ThumbResponse {
            status: 200,
            content_type: Some("image/svg+xml"),
            body,
        }
    }

    fn not_found() -> ThumbResponse {
        ThumbResponse {
            status: 404,
            content_type: None,
            body: Vec::new(),
        }
    }

    /// The request path for `key`.
    fn uri(key: &Option<String>) -> String {
        format!("/{}", key.as_deref().expect("a key"))
    }

    /// The list number in `key`, read by the test itself.
    fn list_of(key: &Option<String>) -> u64 {
        let key = key.as_deref().expect("a key");
        key.split('-').next().unwrap().parse().unwrap()
    }

    /// A `make` double: records every path it is given.
    #[derive(Default)]
    struct Calls(Mutex<Vec<PathBuf>>);

    impl Calls {
        fn make<'a>(
            &'a self,
            answer: impl Fn(&Path) -> ThumbAnswer + 'a,
        ) -> impl Fn(&Path) -> ThumbAnswer + 'a {
            move |path| {
                self.0.lock().unwrap().push(path.to_path_buf());
                answer(path)
            }
        }

        /// Answers each file with [`png_of`] it.
        fn png(&self) -> impl Fn(&Path) -> ThumbAnswer + '_ {
            self.make(|path| ThumbAnswer::Png(png_of(path)))
        }

        fn taken(&self) -> Vec<PathBuf> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    // --- parse_key ------------------------------------------------------------

    #[test]
    fn parse_key_accepts_a_list_number_up_to_10_digits_and_an_index_up_to_4() {
        assert_eq!(parse_key("/3-0"), Some((3, 0)));
        assert_eq!(parse_key("/4294967295-1999"), Some((4_294_967_295, 1999)));
        assert_eq!(parse_key("/0-9999"), Some((0, 9999)));
        assert_eq!(parse_key("/9999999999-0"), Some((9_999_999_999, 0)));
    }

    #[test]
    fn parse_key_rejects_every_other_shape_including_encoded_and_path_like_ones() {
        for bad in [
            "/3-x",
            "/-1",
            "/3/0",
            "/3%2D0",
            "/3-0-1",
            "/12345678901-0",
            "/3-12345",
            "/3-0/extra",
            "/3-0?x=1",
            "3-0",
            "/%2Fetc%2Fpasswd",
            "/../a.png",
            "",
            "/",
            "/3-",
            "/-0",
            "/+3-0",
            "/3-+0",
            "/ 3-0",
            "/3-0 ",
            "//3-0",
            // Arabic-Indic digits: digits, but not ASCII ones.
            "/\u{663}-\u{660}",
        ] {
            assert_eq!(parse_key(bad), None, "{bad:?}");
        }
    }

    // --- snapshot -------------------------------------------------------------

    #[test]
    fn a_snapshot_gives_each_image_row_its_index_key_and_a_skipped_row_none() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.png")), None, Some(path("c.svg"))]);
        let n = list_of(&keys[0]);
        assert_eq!(keys, [Some(format!("{n}-0")), None, Some(format!("{n}-2"))]);
    }

    #[test]
    fn a_snapshot_that_keeps_the_paths_and_adds_rows_keeps_the_list_number() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let n = list_of(&first[0]);
        let more = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            None,
            Some(path("d.png")),
        ]);
        assert_eq!(
            more,
            [
                Some(format!("{n}-0")),
                Some(format!("{n}-1")),
                None,
                Some(format!("{n}-3"))
            ]
        );
        // The same rows again keep their keys too.
        assert_eq!(
            thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]),
            first
        );
    }

    #[test]
    fn a_snapshot_that_gives_a_sent_key_another_path_raises_the_list_number() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            Some(path("c.png")),
        ]);
        let n = list_of(&first[0]);
        // `b.png` dropped from the middle: `c.png` would take index 1.
        let second = thumbs.snapshot(&[Some(path("a.png")), Some(path("c.png"))]);
        assert_eq!(
            second,
            [Some(format!("{}-0", n + 1)), Some(format!("{}-1", n + 1))]
        );
        // The old keys are gone, and the new ones name the new rows.
        let calls = Calls::default();
        assert_eq!(thumbs.answer(&uri(&first[2]), calls.png()), not_found());
        assert_eq!(thumbs.answer(&uri(&first[1]), calls.png()), not_found());
        assert_eq!(calls.taken(), Vec::<PathBuf>::new());
        assert_eq!(
            thumbs.answer(&uri(&second[1]), calls.png()),
            png_response(png_of(&path("c.png")))
        );
        assert_eq!(calls.taken(), [path("c.png")]);
    }

    #[test]
    fn a_key_given_up_by_a_skip_cannot_return_with_another_path() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let n = list_of(&first[0]);
        let skipped = thumbs.snapshot(&[Some(path("a.png")), None]);
        assert_eq!(skipped, [Some(format!("{n}-0")), None]);
        let other = thumbs.snapshot(&[Some(path("a.png")), Some(path("x.png"))]);
        assert_eq!(
            other,
            [Some(format!("{}-0", n + 1)), Some(format!("{}-1", n + 1))]
        );
    }

    #[test]
    fn a_key_given_up_by_a_skip_keeps_the_number_when_its_path_returns() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let n = list_of(&first[0]);
        assert_eq!(
            thumbs.snapshot(&[Some(path("a.png")), None]),
            [Some(format!("{n}-0")), None]
        );
        assert_eq!(
            thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]),
            first
        );
    }

    #[test]
    fn new_list_raises_the_list_number_for_the_same_rows() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[Some(path("a.png"))]);
        let n = list_of(&first[0]);
        thumbs.new_list();
        // Until the next snapshot, the old key names nothing.
        let calls = Calls::default();
        assert_eq!(thumbs.answer(&uri(&first[0]), calls.png()), not_found());
        assert_eq!(calls.taken(), Vec::<PathBuf>::new());
        let second = thumbs.snapshot(&[Some(path("a.png"))]);
        assert_eq!(second, [Some(format!("{}-0", n + 1))]);
    }

    // --- answer ---------------------------------------------------------------

    /// Four rows: three images and a skipped one at index 2.
    fn four_rows() -> (Thumbs, Vec<Option<String>>) {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            None,
            Some(path("d.png")),
        ]);
        (thumbs, keys)
    }

    #[test]
    fn the_current_key_of_an_image_row_is_its_png_made_from_that_rows_path() {
        let (thumbs, keys) = four_rows();
        let calls = Calls::default();
        assert_eq!(
            thumbs.answer(&uri(&keys[1]), calls.png()),
            png_response(png_of(&path("b.png")))
        );
        assert_eq!(calls.taken(), [path("b.png")]);
    }

    #[test]
    fn an_old_list_a_missing_or_skipped_row_and_every_bad_shape_are_404_without_a_call() {
        let thumbs = Thumbs::new();
        let old = thumbs.snapshot(&[Some(path("a.png"))]);
        thumbs.new_list();
        let keys = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            None,
            Some(path("d.png")),
        ]);
        let n = list_of(&keys[0]);
        let calls = Calls::default();
        let requests = [
            uri(&old[0]),
            // Past the end.
            format!("/{n}-4"),
            format!("/{n}-9999"),
            // The skipped row.
            format!("/{n}-2"),
            // A list that never was.
            format!("/{}-0", n + 1),
            // Bad shapes around a current key.
            format!("/{n}-x"),
            format!("/{n}/0"),
            format!("/{n}%2D0"),
            format!("/{n}-0-1"),
            format!("/{n}-0/extra"),
            format!("/{n}-0?x=1"),
            format!("{n}-0"),
            format!("/{n}-00000"),
            "/-1".to_owned(),
            "/12345678901-0".to_owned(),
            "/%2Fetc%2Fpasswd".to_owned(),
            "/../a.png".to_owned(),
            String::new(),
        ];
        for request in &requests {
            assert_eq!(
                thumbs.answer(request, calls.png()),
                not_found(),
                "{request:?}"
            );
        }
        assert_eq!(calls.taken(), Vec::<PathBuf>::new());
    }

    #[test]
    fn cannot_show_is_404_and_remembered() {
        let (thumbs, keys) = four_rows();
        let calls = Calls::default();
        let make = calls.make(|_| ThumbAnswer::CannotShow);
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make), not_found());
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make), not_found());
        assert_eq!(calls.taken(), [path("a.png")]);
    }

    #[test]
    fn failed_is_404_and_tried_again() {
        let (thumbs, keys) = four_rows();
        let calls = Calls::default();
        let make = calls.make(|_| ThumbAnswer::Failed);
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make), not_found());
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make), not_found());
        assert_eq!(calls.taken(), [path("a.png"), path("a.png")]);
    }

    /// An SVG answer that names the file it came from.
    fn svg_of(path: &Path) -> Vec<u8> {
        [b"<svg/>".as_slice(), path.as_os_str().as_encoded_bytes()].concat()
    }

    #[test]
    fn an_svg_row_answered_svg_is_served_as_svg_in_any_case_of_the_extension() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[
            Some(path("a.svg")),
            Some(path("B.SVG")),
            Some(path("c.Svg")),
        ]);
        let calls = Calls::default();
        let make = calls.make(|path| ThumbAnswer::Svg(svg_of(path)));
        for (key, name) in keys.iter().zip(["a.svg", "B.SVG", "c.Svg"]) {
            assert_eq!(
                thumbs.answer(&uri(key), &make),
                svg_response(svg_of(&path(name)))
            );
        }
        // Remembered: asked again, nothing is made again.
        for key in &keys {
            assert_eq!(thumbs.answer(&uri(key), &make).status, 200);
        }
        assert_eq!(calls.taken(), [path("a.svg"), path("B.SVG"), path("c.Svg")]);
    }

    #[test]
    fn a_row_not_named_svg_answered_svg_is_404_and_not_remembered() {
        let names = ["a.png", "a.svgz", "svg", "a.svg.png", "asvg"];
        let thumbs = Thumbs::new();
        let rows: Vec<Option<PathBuf>> = names.iter().map(|name| Some(path(name))).collect();
        let keys = thumbs.snapshot(&rows);
        let calls = Calls::default();
        let make = calls.make(|path| ThumbAnswer::Svg(svg_of(path)));
        for _ in 0..2 {
            for key in &keys {
                assert_eq!(thumbs.answer(&uri(key), &make), not_found(), "{key:?}");
            }
        }
        // Not remembered: every request made it again.
        assert_eq!(calls.taken().len(), names.len() * 2);
    }

    #[test]
    fn an_svg_row_answered_png_is_served_as_png() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.svg"))]);
        let calls = Calls::default();
        assert_eq!(
            thumbs.answer(&uri(&keys[0]), calls.png()),
            png_response(png_of(&path("a.svg")))
        );
    }

    #[test]
    fn a_second_request_for_a_key_is_served_from_the_cache() {
        let (thumbs, keys) = four_rows();
        let calls = Calls::default();
        let make = calls.png();
        let first = thumbs.answer(&uri(&keys[0]), &make);
        assert_eq!(first, png_response(png_of(&path("a.png"))));
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make), first);
        // Another key is made on its own.
        assert_eq!(
            thumbs.answer(&uri(&keys[3]), &make),
            png_response(png_of(&path("d.png")))
        );
        assert_eq!(calls.taken(), [path("a.png"), path("d.png")]);
    }

    #[test]
    fn after_new_list_the_same_index_is_made_again() {
        let thumbs = Thumbs::new();
        let first = thumbs.snapshot(&[Some(path("a.png"))]);
        let calls = Calls::default();
        thumbs.answer(&uri(&first[0]), calls.png());
        thumbs.new_list();
        let second = thumbs.snapshot(&[Some(path("a.png"))]);
        assert_eq!(
            thumbs.answer(&uri(&second[0]), calls.png()),
            png_response(png_of(&path("a.png")))
        );
        assert_eq!(calls.taken(), [path("a.png"), path("a.png")]);
    }

    #[test]
    fn a_cached_row_that_became_skipped_is_404() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let calls = Calls::default();
        assert_eq!(thumbs.answer(&uri(&keys[1]), calls.png()).status, 200);
        assert_eq!(
            thumbs.snapshot(&[Some(path("a.png")), None]),
            [keys[0].clone(), None]
        );
        assert_eq!(thumbs.answer(&uri(&keys[1]), calls.png()), not_found());
        assert_eq!(calls.taken(), [path("b.png")]);
    }

    // Review Focus 1: a new list starts while a thumbnail is being made.
    #[test]
    fn an_answer_for_a_list_that_ended_while_it_was_made_is_404_and_not_kept() {
        let thumbs = Thumbs::new();
        let old = thumbs.snapshot(&[Some(path("a.png"))]);
        let calls = Calls::default();
        // The first make sees its list end before it answers.
        let make = calls.make(|_| {
            if calls.0.lock().unwrap().len() == 1 {
                thumbs.new_list();
                ThumbAnswer::Png(b"\x89PNG\r\n\x1a\nold".to_vec())
            } else {
                ThumbAnswer::Png(b"\x89PNG\r\n\x1a\nnew".to_vec())
            }
        });
        assert_eq!(thumbs.answer(&uri(&old[0]), &make), not_found());
        let new = thumbs.snapshot(&[Some(path("a.png"))]);
        assert_ne!(new, old);
        // The new list's key is made again: the old answer was not kept.
        assert_eq!(
            thumbs.answer(&uri(&new[0]), &make),
            png_response(b"\x89PNG\r\n\x1a\nnew".to_vec())
        );
        assert_eq!(thumbs.answer(&uri(&old[0]), &make), not_found());
        assert_eq!(calls.taken(), [path("a.png"), path("a.png")]);
    }

    // --- the request ------------------------------------------------------------

    /// A GET request for `address`, as the webview sends it.
    fn request_for(address: &str) -> tauri::http::Request<Vec<u8>> {
        tauri::http::Request::builder()
            .uri(address)
            .body(Vec::new())
            .expect("a request")
    }

    /// A fresh list of two rows, and the key of the first.
    fn two_rows() -> (Thumbs, String) {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let key = keys[0].clone().expect("a key");
        (thumbs, key)
    }

    #[test]
    fn a_request_for_a_current_key_is_answered_on_both_platform_addresses() {
        let (thumbs, key) = two_rows();
        let calls = Calls::default();
        for host in ["thumb://localhost", "http://thumb.localhost"] {
            let request = request_for(&format!("{host}/{key}"));
            assert_eq!(
                thumbs.answer_request(&request, calls.png()),
                png_response(png_of(&path("a.png"))),
                "{host}"
            );
        }
        // The second was kept from the first.
        assert_eq!(calls.taken(), [path("a.png")]);
    }

    #[test]
    fn a_request_with_a_query_is_404_and_nothing_is_made() {
        let (thumbs, key) = two_rows();
        let calls = Calls::default();
        for query in ["?x=1", "?", "?x=1&y=2", "?%2F"] {
            for host in ["thumb://localhost", "http://thumb.localhost"] {
                let address = format!("{host}/{key}{query}");
                let request = request_for(&address);
                assert_eq!(
                    thumbs.answer_request(&request, calls.png()),
                    not_found(),
                    "{address}"
                );
            }
        }
        assert_eq!(calls.taken(), Vec::<PathBuf>::new());
        // The key itself is current, and the refused requests kept nothing.
        let plain = request_for(&format!("thumb://localhost/{key}"));
        assert_eq!(
            thumbs.answer_request(&plain, calls.png()),
            png_response(png_of(&path("a.png")))
        );
        assert_eq!(calls.taken(), [path("a.png")]);
    }

    #[test]
    fn a_fragment_never_reaches_the_key_check_because_the_uri_drops_it() {
        let (thumbs, key) = two_rows();
        let calls = Calls::default();
        let request = request_for(&format!("thumb://localhost/{key}#frag"));
        // If the `http` crate ever kept a fragment, the path would carry it
        // (or the query would), and this test would fail.
        assert_eq!(request.uri().path(), format!("/{key}"));
        assert_eq!(request.uri().query(), None);
        assert_eq!(
            thumbs.answer_request(&request, calls.png()),
            png_response(png_of(&path("a.png")))
        );
        // A fragment after a query does not make the query go away.
        let both = request_for(&format!("thumb://localhost/{key}?x=1#frag"));
        assert_eq!(thumbs.answer_request(&both, calls.png()), not_found());
    }

    #[test]
    fn a_request_whose_path_is_not_exactly_a_key_is_404_and_nothing_is_made() {
        let (thumbs, key) = two_rows();
        let calls = Calls::default();
        let (list, index) = key.split_once('-').unwrap();
        for address in [
            format!("thumb://localhost/{list}%2D{index}"),
            format!("thumb://localhost/{key}/extra"),
            format!("thumb://localhost//{key}"),
            format!("thumb://localhost/{list}-x"),
            "thumb://localhost/%2Fetc%2Fpasswd".to_owned(),
            "thumb://localhost/../a.png".to_owned(),
            "thumb://localhost".to_owned(),
        ] {
            let request = request_for(&address);
            assert_eq!(
                thumbs.answer_request(&request, calls.png()),
                not_found(),
                "{address}"
            );
        }
        assert_eq!(calls.taken(), Vec::<PathBuf>::new());
    }

    // --- the byte budget of the kept answers ------------------------------------

    #[test]
    fn a_new_thumbs_has_the_64_mib_budget() {
        assert_eq!(MAX_THUMB_CACHE_BYTES, 64 * 1024 * 1024);
        assert_eq!(Thumbs::new().budget, MAX_THUMB_CACHE_BYTES);
    }

    /// Two PNG rows, `a.png` and `b.png`, with a budget of `extra` bytes on
    /// top of their two bodies (negative: below them).
    fn two_rows_with_budget(extra: isize) -> (Thumbs, Vec<Option<String>>) {
        let both = png_of(&path("a.png")).len() + png_of(&path("b.png")).len();
        let thumbs = Thumbs::with_budget(both.checked_add_signed(extra).unwrap());
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        (thumbs, keys)
    }

    #[test]
    fn answers_that_bring_the_total_to_exactly_the_budget_are_kept() {
        let (thumbs, keys) = two_rows_with_budget(0);
        let calls = Calls::default();
        for _ in 0..2 {
            assert_eq!(
                thumbs.answer(&uri(&keys[0]), calls.png()),
                png_response(png_of(&path("a.png")))
            );
            assert_eq!(
                thumbs.answer(&uri(&keys[1]), calls.png()),
                png_response(png_of(&path("b.png")))
            );
        }
        // Asked twice, each made once.
        assert_eq!(calls.taken(), [path("a.png"), path("b.png")]);
    }

    #[test]
    fn an_answer_one_byte_over_the_budget_is_served_but_not_kept() {
        let (thumbs, keys) = two_rows_with_budget(-1);
        let calls = Calls::default();
        for _ in 0..2 {
            assert_eq!(
                thumbs.answer(&uri(&keys[0]), calls.png()),
                png_response(png_of(&path("a.png")))
            );
            // Served all the same, with its type and bytes.
            assert_eq!(
                thumbs.answer(&uri(&keys[1]), calls.png()),
                png_response(png_of(&path("b.png")))
            );
        }
        // `a.png` was kept, `b.png` did not fit and was made again.
        assert_eq!(calls.taken(), [path("a.png"), path("b.png"), path("b.png")]);
    }

    #[test]
    fn after_new_list_an_answer_that_was_over_the_budget_is_kept() {
        let (thumbs, keys) = two_rows_with_budget(-1);
        let calls = Calls::default();
        thumbs.answer(&uri(&keys[0]), calls.png());
        thumbs.answer(&uri(&keys[1]), calls.png());
        thumbs.answer(&uri(&keys[1]), calls.png());
        assert_eq!(calls.taken(), [path("a.png"), path("b.png"), path("b.png")]);
        thumbs.new_list();
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        // `b.png` first: alone it fits the budget of the new list.
        for _ in 0..2 {
            assert_eq!(
                thumbs.answer(&uri(&keys[1]), calls.png()),
                png_response(png_of(&path("b.png")))
            );
        }
        assert_eq!(calls.taken(), [path("b.png")]);
    }

    #[test]
    fn cannot_show_is_kept_when_the_budget_is_used_up() {
        let budget = png_of(&path("a.png")).len();
        let thumbs = Thumbs::with_budget(budget);
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("broken.png"))]);
        let calls = Calls::default();
        let make = calls.make(|path| {
            if path.ends_with("broken.png") {
                ThumbAnswer::CannotShow
            } else {
                ThumbAnswer::Png(png_of(path))
            }
        });
        // The budget is used up exactly.
        assert_eq!(thumbs.answer(&uri(&keys[0]), &make).status, 200);
        for _ in 0..2 {
            assert_eq!(thumbs.answer(&uri(&keys[1]), &make), not_found());
        }
        assert_eq!(calls.taken(), [path("a.png"), path("broken.png")]);
    }

    // --- the pool -------------------------------------------------------------

    /// A `make` double that holds each call until its path is let through,
    /// and counts how many calls are inside at once.
    #[derive(Default)]
    struct Gate {
        state: Mutex<GateState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct GateState {
        inside: usize,
        peak: usize,
        calls: Vec<PathBuf>,
        open: HashSet<PathBuf>,
        all_open: bool,
    }

    impl Gate {
        fn make(&self, path: &Path) -> ThumbAnswer {
            let mut state = self.state.lock().unwrap();
            state.inside += 1;
            state.peak = state.peak.max(state.inside);
            state.calls.push(path.to_path_buf());
            self.changed.notify_all();
            let (mut state, _) = self
                .changed
                .wait_timeout_while(state, WAIT, |state| {
                    !state.all_open && !state.open.contains(path)
                })
                .unwrap();
            state.inside -= 1;
            self.changed.notify_all();
            ThumbAnswer::Png(png_of(path))
        }

        /// Whether `done` became true within [`WAIT`].
        fn wait_for(&self, done: impl Fn(&GateState) -> bool) -> bool {
            let state = self.state.lock().unwrap();
            let (_state, result) = self
                .changed
                .wait_timeout_while(state, WAIT, |state| !done(state))
                .unwrap();
            !result.timed_out()
        }

        fn open(&self, path: &Path) {
            self.state.lock().unwrap().open.insert(path.to_path_buf());
            self.changed.notify_all();
        }

        fn open_all(&self) {
            self.state.lock().unwrap().all_open = true;
            self.changed.notify_all();
        }

        fn calls(&self) -> Vec<PathBuf> {
            self.state.lock().unwrap().calls.clone()
        }

        fn peak(&self) -> usize {
            self.state.lock().unwrap().peak
        }
    }

    /// Whether `done` became true within [`WAIT`], asked every millisecond.
    fn eventually(done: impl Fn() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < WAIT {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        done()
    }

    #[test]
    fn two_requests_make_their_thumbnails_at_the_same_time() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let gate = Gate::default();
        std::thread::scope(|scope| {
            let requests: Vec<_> = keys
                .iter()
                .map(|key| {
                    let (thumbs, gate, uri) = (&thumbs, &gate, uri(key));
                    scope.spawn(move || thumbs.answer(&uri, |path| gate.make(path)))
                })
                .collect();
            let both_inside = gate.wait_for(|state| state.inside == 2);
            gate.open_all();
            assert!(
                both_inside,
                "the second request did not start beside the first"
            );
            for request in requests {
                assert_eq!(request.join().unwrap().status, 200);
            }
        });
        assert_eq!(gate.peak(), 2);
    }

    #[test]
    fn at_most_two_makes_run_at_once_and_a_third_request_waits() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            Some(path("c.png")),
        ]);
        let gate = Gate::default();
        std::thread::scope(|scope| {
            let requests: Vec<_> = keys
                .iter()
                .map(|key| {
                    let (thumbs, gate, uri) = (&thumbs, &gate, uri(key));
                    scope.spawn(move || thumbs.answer(&uri, |path| gate.make(path)))
                })
                .collect();
            let two_inside = gate.wait_for(|state| state.inside == 2);
            let third_waits = eventually(|| thumbs.waiting() == 1);
            let calls_while_full = gate.calls().len();
            gate.open_all();
            assert!(two_inside);
            assert!(third_waits, "the third request did not wait for a worker");
            assert_eq!(calls_while_full, 2);
            for request in requests {
                assert_eq!(request.join().unwrap().status, 200);
            }
        });
        assert_eq!(gate.peak(), 2);
        assert_eq!(gate.calls().len(), 3);
    }

    #[test]
    fn a_request_waiting_for_a_worker_when_its_list_ends_is_404_and_starts_none() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[
            Some(path("a.png")),
            Some(path("b.png")),
            Some(path("c.png")),
        ]);
        let gate = Gate::default();
        std::thread::scope(|scope| {
            let requests: Vec<_> = keys
                .iter()
                .map(|key| {
                    let (thumbs, gate, uri) = (&thumbs, &gate, uri(key));
                    scope.spawn(move || thumbs.answer(&uri, |path| gate.make(path)))
                })
                .collect();
            let ready =
                gate.wait_for(|state| state.inside == 2) && eventually(|| thumbs.waiting() == 1);
            thumbs.new_list();
            gate.open_all();
            assert!(ready);
            for request in requests {
                assert_eq!(request.join().unwrap(), not_found());
            }
        });
        // The waiting request started no worker.
        assert_eq!(gate.calls().len(), 2);
    }

    #[test]
    fn a_request_waiting_for_a_worker_whose_key_was_made_meanwhile_is_served_from_the_cache() {
        let thumbs = Thumbs::new();
        let keys = thumbs.snapshot(&[Some(path("a.png")), Some(path("b.png"))]);
        let gate = Gate::default();
        std::thread::scope(|scope| {
            let request = |key: &Option<String>| {
                let (thumbs, gate, uri) = (&thumbs, &gate, uri(key));
                scope.spawn(move || thumbs.answer(&uri, |path| gate.make(path)))
            };
            let first_a = request(&keys[0]);
            let a_inside = gate.wait_for(|state| state.inside == 1);
            let b = request(&keys[1]);
            let b_inside = gate.wait_for(|state| state.inside == 2);
            let second_a = request(&keys[0]);
            let second_waits = eventually(|| thumbs.waiting() == 1);
            // Only `a.png` is let through: the waiting request gets its slot.
            gate.open(&path("a.png"));
            let second = second_a.join().unwrap();
            gate.open_all();
            assert!(a_inside && b_inside && second_waits);
            assert_eq!(second, png_response(png_of(&path("a.png"))));
            assert_eq!(first_a.join().unwrap(), second);
            assert_eq!(b.join().unwrap().status, 200);
        });
        assert_eq!(gate.calls(), [path("a.png"), path("b.png")]);
    }

    // --- the HTTP response ----------------------------------------------------

    #[test]
    fn a_response_becomes_http_with_its_status_type_and_body() {
        let png = png_response(vec![1, 2, 3]).into_http();
        assert_eq!(png.status().as_u16(), 200);
        assert_eq!(png.headers().len(), 3, "the type, the policy, nosniff");
        assert_eq!(png.headers()[CONTENT_TYPE], "image/png");
        assert_eq!(png.body(), &[1, 2, 3]);

        let svg = svg_response(b"<svg/>".to_vec()).into_http();
        assert_eq!(svg.status().as_u16(), 200);
        assert_eq!(svg.headers()[CONTENT_TYPE], "image/svg+xml");
        assert_eq!(svg.body(), b"<svg/>");

        let missing = not_found().into_http();
        assert_eq!(missing.status().as_u16(), 404);
        assert_eq!(missing.headers().len(), 2, "the policy and nosniff only");
        assert!(missing.headers().get(CONTENT_TYPE).is_none());
        assert!(missing.body().is_empty());
        assert_eq!(ThumbResponse::not_found(), not_found());
    }

    /// The two headers every answer carries, with exactly these values.
    fn has_policy_and_nosniff(response: &Response<Vec<u8>>) {
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'none'; style-src 'unsafe-inline'"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }

    #[test]
    fn a_thumb_200_carries_the_csp_and_nosniff() {
        has_policy_and_nosniff(&png_response(vec![1]).into_http());
        has_policy_and_nosniff(&svg_response(b"<svg/>".to_vec()).into_http());
    }

    #[test]
    fn a_thumb_404_carries_the_csp_and_nosniff() {
        has_policy_and_nosniff(&not_found().into_http());
    }

    #[test]
    fn a_response_with_an_impossible_status_becomes_an_empty_404() {
        let impossible = ThumbResponse {
            status: 1000,
            content_type: Some("image/png"),
            body: vec![1, 2, 3],
        }
        .into_http();
        assert_eq!(impossible.status().as_u16(), 404);
        assert_eq!(impossible.headers().len(), 2, "the policy and nosniff only");
        has_policy_and_nosniff(&impossible);
        assert!(impossible.body().is_empty());
    }
}
