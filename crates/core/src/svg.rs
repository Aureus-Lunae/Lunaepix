//! SVG optimisation with oxvg, a Rust port of svgo.
//!
//! The settings are the Step 2 evaluation's, the calling sequence of the
//! `oxvg optimise` CLI: a roxmltree parse, `Jobs::default()` (svgo's
//! preset-default) and the default writer options (minified).
//!
//! An SVG is untrusted text, so the order matters. Every check below runs on
//! the input before oxvg sees it, and nothing is written unless all pass:
//!
//! 1. The bytes must be UTF-8.
//! 2. [`check_entities`]: an upper bound on what the entity references can
//!    expand to, before the document is parsed (the expansion happens while
//!    parsing).
//! 3. [`parse`]: roxmltree with [`parsing_options`]. DTDs are allowed,
//!    because Illustrator puts its namespaces in internal entities. External
//!    entities are never resolved: there is no entity resolver, so a
//!    `SYSTEM` entity has no content and a reference to it is an error.
//!    roxmltree's own `nodes_limit` caps the node count.
//! 4. [`check_root`]: the root element is `<svg>`.
//! 5. [`check_depth`]: the nesting depth, measured iteratively on the very
//!    document that is then handed to oxvg (oxvg recurses; see
//!    [`MAX_SVG_DEPTH`]).
//!
//! [`check`] runs these five steps and nothing else, for a caller that only
//! wants to know whether the file would be taken (the probe).
//!
//! Nothing here opens a file or a connection: roxmltree has no I/O and
//! oxvg's crates contain no filesystem or network code. Hrefs, `<use>`
//! targets and DOCTYPE URLs are only text. A `<script>` is kept, as svgo's
//! preset-default keeps it; it is never run.
//!
//! The output is parsed again and must still be an SVG ([`check_output`]).
//! If it is not smaller than the input, the input bytes are returned, so an
//! already-optimised file is copied as it is and never grows.

use oxvg_ast::parse::roxmltree::parse_tree;
use oxvg_ast::serialize::Node as _;
use oxvg_ast::visitor::Info;
use oxvg_ast::xmlwriter::Options;
use oxvg_optimiser::Jobs;
use roxmltree::{Document, ParsingOptions};

use crate::plan::{MAX_SVG_DEPTH, MAX_SVG_ENTITY_BYTES, MAX_SVG_NODES};

/// How many entity values one reference in the body can pull in at most:
/// itself plus the 255 nested references roxmltree allows inside one
/// reference (its `LoopDetector`; the 256th is an `EntityReferenceLoop`).
const VALUES_PER_REFERENCE: u64 = 256;

/// Optimise SVG bytes. The error is a message for the user: the content is
/// not an SVG, the file is outside a limit, or oxvg failed.
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let text = utf8(bytes)?;
    let doc = checked_document(text)?;
    let optimized = run_oxvg(&doc)?;
    check_output(&optimized)?;
    if optimized.len() < bytes.len() {
        Ok(optimized)
    } else {
        Ok(bytes.to_vec())
    }
}

/// Run every check [`optimize`] makes on the input before oxvg sees it
/// (steps 1 to 5 above), and nothing else: for a caller that wants to know
/// whether the file would be taken, with the messages `optimize` gives.
pub(crate) fn check(bytes: &[u8]) -> Result<(), String> {
    checked_document(utf8(bytes)?).map(drop)
}

/// Step 1: the bytes as text.
fn utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes)
        .map_err(|_| "not an SVG file (the content is not UTF-8 text)".to_owned())
}

/// Steps 2 to 5: the parsed document, once the entity bound, the parse, the
/// root element and the depth have passed.
fn checked_document(text: &str) -> Result<Document<'_>, String> {
    check_entities(text)?;
    let doc = parse(text)?;
    check_root(&doc)?;
    check_depth(&doc)?;
    Ok(doc)
}

/// The options for every parse: DTDs allowed, roxmltree's node limit, and
/// no entity resolver, so external entities are never fetched or read.
fn parsing_options<'input>() -> ParsingOptions<'input> {
    ParsingOptions {
        allow_dtd: true,
        nodes_limit: MAX_SVG_NODES,
        entity_resolver: None,
    }
}

/// Refuse a file whose entity references could expand past
/// [`MAX_SVG_ENTITY_BYTES`], from an upper bound computed before the real
/// parse.
///
/// Without an `<!ENTITY` declaration only the predefined entities exist, one
/// character each, so there is nothing to bound. Otherwise:
///
/// - Where the prolog ends is found by roxmltree itself, on a probe copy in
///   which every `&` is made inert, so nothing expands; the copy has the same
///   length and the same markup, so its root element starts at the same byte.
/// - Every entity value is a literal inside the DTD, so none is longer than
///   the prolog (everything before the root element); external entities have
///   no value here.
/// - Each reference in the body pulls in one value, or up to
///   [`VALUES_PER_REFERENCE`] values when the prolog holds any `&` at all:
///   a declaration may then reference another entity. The test is any `&`
///   rather than a parsed reference, so it also fires for a character
///   reference or a predefined entity in a value, and for an `&` in a
///   comment. None of those nest (roxmltree expands `&#38;a;` in a value to
///   the text `&a;`, never to a reference), so counting them only makes the
///   bound stricter.
/// - Every reference in the body starts with `&`. Character references and
///   the five predefined entities there ([`NEVER_EXPANDS`]) are one
///   character each and are not counted. Any other `&` is counted, even
///   where it is no reference (in a comment or CDATA, say), which again
///   only makes the bound stricter.
///
/// roxmltree refuses a "billion laughs" itself, but not many references to
/// one large value, which grows quadratically. This bound depends on
/// roxmltree's limit of 255 nested references inside one reference, which is
/// why roxmltree is pinned with `=` and the `roxmltree_*` tests below check
/// that limit.
fn check_entities(text: &str) -> Result<(), String> {
    if !text.contains("<!ENTITY") {
        return Ok(());
    }
    let probe = text.replace('&', "_");
    let probe = Document::parse_with_options(&probe, parsing_options()).map_err(read_error)?;
    let (prolog, body) = text.split_at(probe.root_element().range().start);
    let per_reference = prolog.len() as u64
        * if prolog.contains('&') {
            VALUES_PER_REFERENCE
        } else {
            1
        };
    if references(body).saturating_mul(per_reference) > MAX_SVG_ENTITY_BYTES {
        return Err(format!(
            "too large: the SVG's entity references could expand to more than {} MiB",
            MAX_SVG_ENTITY_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// What may follow an `&` without it being a reference that can expand: a
/// character reference, or one of the five predefined entities. roxmltree
/// turns those five into characters before it looks at the DTD
/// (`Stream::consume_reference`), so a declaration cannot redefine them.
const NEVER_EXPANDS: [&[u8]; 6] = [b"#", b"amp;", b"lt;", b"gt;", b"quot;", b"apos;"];

/// Every `&` that does not start a reference in [`NEVER_EXPANDS`].
fn references(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let count = bytes
        .iter()
        .enumerate()
        .filter(|&(at, &byte)| {
            byte == b'&'
                && !NEVER_EXPANDS
                    .iter()
                    .any(|tail| bytes[at + 1..].starts_with(tail))
        })
        .count();
    count as u64
}

/// Parse `text` with [`parsing_options`].
fn parse(text: &str) -> Result<Document<'_>, String> {
    Document::parse_with_options(text, parsing_options()).map_err(read_error)
}

/// A roxmltree error as a message for the user.
fn read_error(error: roxmltree::Error) -> String {
    match error {
        roxmltree::Error::NodesLimitReached => {
            format!("SVG too large: more than {MAX_SVG_NODES} nodes")
        }
        other => format!("could not read the SVG: it is not well-formed XML ({other})"),
    }
}

/// The root element must be `<svg>`, with or without a namespace prefix.
fn check_root(doc: &Document) -> Result<(), String> {
    let name = doc.root_element().tag_name().name();
    if name == "svg" {
        Ok(())
    } else {
        Err(format!(
            "not an SVG file (the root element is <{name}>, not <svg>)"
        ))
    }
}

/// Refuse a document whose elements nest deeper than [`MAX_SVG_DEPTH`] (the
/// root element is level 1). Iterative, so any depth is safe to measure.
fn check_depth(doc: &Document) -> Result<(), String> {
    let mut pending = vec![(doc.root_element(), 1)];
    while let Some((element, depth)) = pending.pop() {
        if depth > MAX_SVG_DEPTH {
            return Err(format!(
                "SVG nested too deeply: more than {MAX_SVG_DEPTH} levels of elements"
            ));
        }
        pending.extend(
            element
                .children()
                .filter(|child| child.is_element())
                .map(|child| (child, depth + 1)),
        );
    }
    Ok(())
}

/// Hand the checked document to oxvg, run svgo's preset-default and write
/// the result minified.
fn run_oxvg(doc: &Document) -> Result<Vec<u8>, String> {
    parse_tree(doc, |dom, allocator| -> Result<Vec<u8>, String> {
        Jobs::default()
            .run(dom, &Info::new(allocator))
            .map_err(|error| format!("could not optimise the SVG: {error}"))?;
        dom.serialize_into(Vec::new(), Options::default())
            .map_err(|error| format!("could not write the optimised SVG: {error}"))
    })
    .map_err(|error| format!("could not read the SVG: {error}"))?
}

/// The optimised bytes must parse again, with the same options, as an SVG.
fn check_output(bytes: &[u8]) -> Result<(), String> {
    let failed = |why: String| format!("the optimised SVG does not parse as an SVG: {why}");
    let text = std::str::from_utf8(bytes).map_err(|error| failed(error.to_string()))?;
    let doc = parse(text).map_err(failed)?;
    check_root(&doc).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG_NS: &str = r#"xmlns="http://www.w3.org/2000/svg""#;

    /// `<svg>` holding `levels - 1` nested groups: `levels` element levels.
    fn nested(levels: usize) -> String {
        let inner = levels - 1;
        format!(
            "<svg {SVG_NS}>{}{}</svg>",
            "<g>".repeat(inner),
            "</g>".repeat(inner)
        )
    }

    /// A DOCTYPE that declares `decls`, then `<svg>` holding `body` in a
    /// `<desc>`. Returns the document and the length of everything before
    /// the root element.
    fn with_dtd(decls: &str, body: &str) -> (String, usize) {
        let prolog = format!("<!DOCTYPE svg [{decls}]>");
        let doc = format!("{prolog}<svg {SVG_NS}><desc>{body}</desc></svg>");
        (doc, prolog.len())
    }

    fn budget() -> usize {
        MAX_SVG_ENTITY_BYTES as usize
    }

    // --- the content check --------------------------------------------------------

    #[test]
    fn rejects_bytes_that_are_not_utf8_text() {
        let err = optimize(&[0x89, b'P', b'N', b'G', 0xFF, 0xFE]).unwrap_err();
        assert!(err.contains("not an SVG file"), "{err}");
    }

    #[test]
    fn accepts_a_root_svg_element_with_or_without_a_prefix() {
        let plain = format!("<svg {SVG_NS}/>");
        assert_eq!(check_root(&parse(&plain).unwrap()), Ok(()));
        let prefixed = r#"<s:svg xmlns:s="http://www.w3.org/2000/svg"/>"#;
        assert_eq!(check_root(&parse(prefixed).unwrap()), Ok(()));
    }

    #[test]
    fn rejects_well_formed_xml_whose_root_is_not_svg() {
        let doc = r#"<html xmlns="http://www.w3.org/1999/xhtml"><svg/></html>"#;
        let err = check_root(&parse(doc).unwrap()).unwrap_err();
        assert!(err.contains("not an SVG file"), "{err}");
        assert!(err.contains("<html>"), "{err}");
    }

    #[test]
    fn rejects_text_that_is_not_well_formed_xml() {
        let err = parse("<svg><g></svg>").unwrap_err();
        assert!(err.contains("not well-formed XML"), "{err}");
    }

    // --- the node limit (roxmltree's own) ------------------------------------------------

    /// `<svg>` holding `groups` empty groups, with no whitespace between.
    fn flat(groups: usize) -> String {
        format!("<svg {SVG_NS}>{}</svg>", "<g/>".repeat(groups))
    }

    #[test]
    fn parses_a_document_of_exactly_the_node_limit() {
        // The document node and <svg> are nodes too.
        let text = flat(MAX_SVG_NODES as usize - 2);
        let doc = parse(&text).unwrap();
        assert_eq!(doc.descendants().count(), MAX_SVG_NODES as usize);
    }

    #[test]
    fn refuses_a_document_one_node_over_the_limit() {
        let err = parse(&flat(MAX_SVG_NODES as usize - 1)).unwrap_err();
        assert!(err.contains("too large"), "{err}");
        assert!(err.contains("1000000 nodes"), "{err}");
    }

    // --- the depth limit -------------------------------------------------------------

    #[test]
    fn accepts_nesting_of_exactly_the_depth_limit() {
        let text = nested(MAX_SVG_DEPTH);
        assert_eq!(check_depth(&parse(&text).unwrap()), Ok(()));
    }

    #[test]
    fn refuses_nesting_one_level_over_the_depth_limit() {
        let text = nested(MAX_SVG_DEPTH + 1);
        let err = check_depth(&parse(&text).unwrap()).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
        assert!(err.contains("128 levels"), "{err}");
    }

    #[test]
    fn counts_the_depth_that_entity_markup_adds() {
        // The text nests 120 levels; the entity adds 20 more inside them. The
        // check must see the parsed document, not the text.
        let deep = format!("{}{}", "<g>".repeat(20), "</g>".repeat(20));
        let text = format!(
            "<!DOCTYPE svg [<!ENTITY deep \"{deep}\">]><svg {SVG_NS}>{}&deep;{}</svg>",
            "<g>".repeat(119),
            "</g>".repeat(119)
        );
        let doc = parse(&text).unwrap();
        assert!(check_depth(&doc).is_err());
        // Without the reference the same text is within the limit.
        let without = text.replace("&deep;", "");
        assert_eq!(check_depth(&parse(&without).unwrap()), Ok(()));
    }

    // --- the entity expansion estimate -----------------------------------------------------

    #[test]
    fn needs_no_estimate_when_no_entity_is_declared() {
        // Only the five predefined entities exist; each expands to one character.
        let text = format!(
            "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x.dtd\"><svg {SVG_NS}><desc>{}</desc></svg>",
            "&amp;".repeat(100_000)
        );
        assert_eq!(check_entities(&text), Ok(()));
    }

    #[test]
    fn accepts_flat_entity_references_up_to_the_budget() {
        let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let refs = budget() / prolog;
        let (text, _) = with_dtd(&decls, &"&a;".repeat(refs));
        assert_eq!(check_entities(&text), Ok(()));
    }

    #[test]
    fn refuses_flat_entity_references_one_past_the_budget() {
        let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let refs = budget() / prolog + 1;
        let (text, _) = with_dtd(&decls, &"&a;".repeat(refs));
        let err = check_entities(&text).unwrap_err();
        assert!(err.contains("entity references"), "{err}");
        assert!(err.contains("16 MiB"), "{err}");
    }

    #[test]
    fn allows_256_times_less_when_entities_reference_entities() {
        // A reference inside a declaration can resolve 255 more references
        // (roxmltree's own limit), so each body reference counts 256 times.
        let decls = format!("<!ENTITY a \"{}\"><!ENTITY b \"&a;&a;\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let refs = budget() / (256 * prolog);
        let (within, _) = with_dtd(&decls, &"&b;".repeat(refs));
        assert_eq!(check_entities(&within), Ok(()));
        let (over, _) = with_dtd(&decls, &"&b;".repeat(refs + 1));
        assert!(check_entities(&over).is_err());
    }

    #[test]
    fn does_not_count_character_references() {
        let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let many = budget() / prolog + 1;
        let (chars, _) = with_dtd(&decls, &"&#65;".repeat(many));
        assert_eq!(check_entities(&chars), Ok(()));
        // The same count of entity references is over the budget.
        let (refs, _) = with_dtd(&decls, &"&a;".repeat(many));
        assert!(check_entities(&refs).is_err());
    }

    #[test]
    fn counts_any_ampersand_in_the_prolog_as_nesting_even_one_that_cannot_nest() {
        // roxmltree expands `&#38;a;` in b's value to the text `&a;`, never to
        // a reference, so b does not nest. The check counts it anyway, which
        // only makes the bound stricter. The same length of prolog with a
        // plain character instead stays flat.
        let flat = format!(
            "<!ENTITY a \"{}\"><!ENTITY b \"x#38;a;\">",
            "x".repeat(1000)
        );
        let nesting = flat.replace("x#38;", "&#38;");
        assert_eq!(flat.len(), nesting.len());
        let (_, prolog) = with_dtd(&nesting, "");
        let refs = budget() / prolog;
        let (within, _) = with_dtd(&flat, &"&b;".repeat(refs));
        assert_eq!(check_entities(&within), Ok(()));
        let (over, _) = with_dtd(&nesting, &"&b;".repeat(refs));
        assert!(check_entities(&over).is_err());
    }

    #[test]
    fn does_not_count_the_five_predefined_entities() {
        // roxmltree reads these as characters before it looks at the DTD, so
        // a declaration cannot redefine them and they never expand.
        let decls = format!(
            "<!ENTITY amp \"{0}\"><!ENTITY lt \"{0}\"><!ENTITY a \"{0}\">",
            "x".repeat(1000)
        );
        let (_, prolog) = with_dtd(&decls, "");
        let many = budget() / prolog + 1;
        for predefined in ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"] {
            let (text, _) = with_dtd(&decls, &predefined.repeat(many));
            assert_eq!(check_entities(&text), Ok(()), "{predefined}");
        }
    }

    #[test]
    fn still_counts_a_declared_entity_whose_name_starts_like_a_predefined_one() {
        let decls = format!("<!ENTITY ampx \"{}\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let many = budget() / prolog + 1;
        let (text, _) = with_dtd(&decls, &"&ampx;".repeat(many));
        assert!(check_entities(&text).is_err());
    }

    #[test]
    fn reports_xml_errors_found_while_estimating() {
        let (text, _) = with_dtd("<!ENTITY a \"x\">", "<unclosed>");
        let err = check_entities(&text).unwrap_err();
        assert!(err.contains("not well-formed XML"), "{err}");
    }

    // --- roxmltree's behaviour that check_entities relies on ------------------------------
    //
    // These run roxmltree itself, with our options. If an upgrade changes
    // its limit or how it reads values, they fail, and the bound in
    // check_entities (VALUES_PER_REFERENCE) must be re-derived.

    /// One top-level reference to r, whose value holds `inner` references
    /// to a, in text (`<desc>`) or in an attribute.
    fn one_reference_resolving(inner: usize, in_attribute: bool) -> String {
        let decls = format!("<!ENTITY a \"x\"><!ENTITY r \"{}\">", "&a;".repeat(inner));
        if in_attribute {
            format!("<!DOCTYPE svg [{decls}]><svg {SVG_NS} id=\"&r;\"/>")
        } else {
            format!("<!DOCTYPE svg [{decls}]><svg {SVG_NS}><desc>&r;</desc></svg>")
        }
    }

    fn roxmltree_parse(text: &str) -> Result<(), roxmltree::Error> {
        Document::parse_with_options(text, parsing_options()).map(drop)
    }

    #[test]
    fn roxmltree_lets_one_reference_resolve_255_nested_references_as_check_entities_assumes() {
        assert_eq!(VALUES_PER_REFERENCE, 1 + 255);
        for in_attribute in [false, true] {
            let text = one_reference_resolving(255, in_attribute);
            assert!(roxmltree_parse(&text).is_ok(), "attribute: {in_attribute}");
        }
    }

    #[test]
    fn roxmltree_refuses_one_reference_that_resolves_256_as_check_entities_assumes() {
        for in_attribute in [false, true] {
            let text = one_reference_resolving(256, in_attribute);
            assert!(
                matches!(
                    roxmltree_parse(&text),
                    Err(roxmltree::Error::EntityReferenceLoop(_))
                ),
                "attribute: {in_attribute}"
            );
        }
    }

    #[test]
    fn roxmltree_reads_a_character_reference_in_a_value_as_text_never_as_a_reference() {
        // `&#38;a;` in b's value becomes the text "&a;". Were it read as a
        // reference, the undeclared a would be an error.
        let text =
            format!("<!DOCTYPE svg [<!ENTITY b \"&#38;a;\">]><svg {SVG_NS}><desc>&b;</desc></svg>");
        let doc = Document::parse_with_options(&text, parsing_options()).unwrap();
        let desc = doc
            .descendants()
            .find(|n| n.tag_name().name() == "desc")
            .unwrap();
        assert_eq!(desc.text(), Some("&a;"));
        // A real reference to the undeclared a is that error.
        let real = text.replace("&#38;a;", "&a;");
        assert!(matches!(
            roxmltree_parse(&real),
            Err(roxmltree::Error::UnknownEntityReference(..))
        ));
    }

    // --- the parser never resolves external entities -----------------------------------------

    #[test]
    fn parses_with_dtds_allowed_and_no_entity_resolver() {
        let options = parsing_options();
        assert!(options.allow_dtd);
        assert!(options.entity_resolver.is_none());
        assert_eq!(options.nodes_limit, MAX_SVG_NODES);
    }

    // --- the not-smaller fallback --------------------------------------------------------

    /// The fixture of `keeps_the_original_bytes_when_the_optimised_svg_is_not_smaller`
    /// in tests/optimize_svg.rs; keep the two identical.
    const NOT_SHRINKABLE: &str =
        r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h9v9z"/></svg>"#;

    #[test]
    fn the_not_shrinkable_fixture_makes_oxvg_return_other_bytes_that_are_not_shorter() {
        // Guard for the integration test: if oxvg returned the input bytes
        // themselves, that test would pass without the fallback ever running.
        let output = run_oxvg(&parse(NOT_SHRINKABLE).unwrap()).unwrap();
        assert_ne!(output, NOT_SHRINKABLE.as_bytes(), "oxvg changed nothing");
        assert!(
            output.len() >= NOT_SHRINKABLE.len(),
            "oxvg shrank it to {}",
            String::from_utf8_lossy(&output)
        );
        // And the fallback then hands back the input, not oxvg's bytes.
        assert_eq!(
            optimize(NOT_SHRINKABLE.as_bytes()).unwrap(),
            NOT_SHRINKABLE.as_bytes()
        );
    }

    // --- the output check ----------------------------------------------------------------

    #[test]
    fn accepts_output_that_parses_as_svg() {
        assert_eq!(check_output(format!("<svg {SVG_NS}/>").as_bytes()), Ok(()));
    }

    #[test]
    fn rejects_output_that_does_not_parse_as_svg() {
        for bad in [&b"<svg"[..], b"<g/>", b"\xFF\xFE"] {
            let err = check_output(bad).unwrap_err();
            assert!(err.contains("the optimised SVG"), "{err}");
        }
    }

    // --- check: the front half of optimize ---------------------------------------------------

    #[test]
    fn check_accepts_an_svg_that_optimize_accepts() {
        let plain = format!("<svg {SVG_NS}/>");
        assert_eq!(check(plain.as_bytes()), Ok(()));
        assert!(optimize(plain.as_bytes()).is_ok());
        let deepest = nested(MAX_SVG_DEPTH);
        assert_eq!(check(deepest.as_bytes()), Ok(()));
        let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
        let (text, _) = with_dtd(&decls, "&a;&a;");
        assert_eq!(check(text.as_bytes()), Ok(()));
    }

    #[test]
    fn check_refuses_what_optimize_refuses_with_the_same_message() {
        let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
        let (_, prolog) = with_dtd(&decls, "");
        let (bomb, _) = with_dtd(&decls, &"&a;".repeat(budget() / prolog + 1));
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("not utf-8", vec![0x89, b'P', b'N', b'G', 0xFF, 0xFE]),
            ("not well-formed", b"<svg><g></svg>".to_vec()),
            (
                "root is not svg",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><svg/></html>"#.to_vec(),
            ),
            ("too deep", nested(MAX_SVG_DEPTH + 1).into_bytes()),
            ("entity bomb", bomb.into_bytes()),
            (
                "too many nodes",
                flat(MAX_SVG_NODES as usize - 1).into_bytes(),
            ),
        ];
        for (what, bytes) in cases {
            let expected = optimize(&bytes).unwrap_err();
            assert_eq!(check(&bytes), Err(expected), "{what}");
        }
    }
}
