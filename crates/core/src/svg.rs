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
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "not an SVG file (the content is not UTF-8 text)".to_owned())?;
    check_entities(text)?;
    let doc = parse(text)?;
    check_root(&doc)?;
    check_depth(&doc)?;
    let optimized = run_oxvg(&doc)?;
    check_output(&optimized)?;
    if optimized.len() < bytes.len() {
        Ok(optimized)
    } else {
        Ok(bytes.to_vec())
    }
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
///   a declaration may then reference another entity. A character
///   reference counts here too (`&#38;a;` in a value reads as `&a;` to some
///   parsers), and so does an `&` in a comment; both only raise the bound.
/// - Every reference in the body starts with `&`. Character references
///   (`&#`) there expand to one character and are not counted. Any other
///   `&` is counted, even where it is no reference (in a comment or CDATA,
///   say), which again only raises the bound.
///
/// roxmltree refuses a "billion laughs" itself, but not many references to
/// one large value, which grows quadratically.
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

/// Every `&` that does not start a character reference.
fn references(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let count = bytes
        .iter()
        .enumerate()
        .filter(|&(at, &byte)| byte == b'&' && bytes.get(at + 1) != Some(&b'#'))
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
    fn counts_a_character_reference_inside_a_declaration_as_nesting() {
        // `&#38;` is `&`: the value of b may read as a reference to a. The
        // same length of prolog with a plain character instead stays flat.
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
    fn reports_xml_errors_found_while_estimating() {
        let (text, _) = with_dtd("<!ENTITY a \"x\">", "<unclosed>");
        let err = check_entities(&text).unwrap_err();
        assert!(err.contains("not well-formed XML"), "{err}");
    }

    // --- the parser never resolves external entities -----------------------------------------

    #[test]
    fn parses_with_dtds_allowed_and_no_entity_resolver() {
        let options = parsing_options();
        assert!(options.allow_dtd);
        assert!(options.entity_resolver.is_none());
        assert_eq!(options.nodes_limit, MAX_SVG_NODES);
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
}
