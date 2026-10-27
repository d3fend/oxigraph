//! Data-driven conformance sweep over the W3C XML Schema Test Suite's
//! `Regex_w3c` testSet -- the pattern-facet / `xs:pattern` cluster of the
//! XSD Schema Test Suite (`xsdtests`), 2590 `testGroup` entries under
//! `msMeta/Regex_w3c.xml`, each pairing a `schemaTest` (is the pattern
//! itself a syntactically valid XSD regular expression?) with zero or more
//! `instanceTest`s (does a given string match it, matching the whole
//! string, exactly as `Regex::xsd` requires?).
//!
//! # How this table was built
//!
//! For every `testGroup`: the pattern is the `value` of the single
//! `xsd:pattern` facet inside the `schemaDocument` it references; the
//! tested value(s) come from the corresponding `instanceDocument`, taken
//! from (in order of preference) an `att` attribute, a `value` attribute,
//! or the element's own text content, on every `<elem>`/`<value>` child of
//! the document root (or the root itself, when it has no such children --
//! the `RegexTest_*` family carries its value directly on the root
//! element). Where a `testGroup` carries more than one applicable
//! `<expected>` for the same test, this crate's target (XSD 1.1, current
//! Unicode) is resolved by: prefer the numerically highest
//! `version="Unicode_X.Y.Z"` entry if any is present (this crate always
//! validates against its one current Unicode snapshot, so of several
//! Unicode-version-conditioned answers the newest-dated one applies);
//! otherwise prefer `version="1.1"`; otherwise the version-less entry. An
//! `<instanceTest version="...">` attribute (distinct from the `<expected>`
//! elements nested inside it) only *excludes* a test when it names a
//! specific, different XSD spec version (`"1.0"`); a `Unicode_X.Y.Z` label
//! observed there once (`reS17`) is documentary only, since its own nested
//! `<expected>` elements already carry the real per-Unicode-version
//! answers.
//!
//! # What is excluded
//!
//! Six `testGroup`s -- `p16` through `p21` -- restrict a type by *two*
//! `xsd:pattern` facets, either at the same derivation step (unioned) or
//! across a named base type's own restriction step (intersected with that
//! step's union). That composition is a schema type-processor concern:
//! deciding it requires knowing the derivation chain, not just compiling
//! one pattern string, so it is outside what a single [`Regex::xsd`] call
//! can be asked to guarantee and is not modelled by the [`Case`] table
//! below at all.
//!
//! Six more -- `reZ003v`, `reZ004v`, `reZ005v`, `reZ005i`, `reZ006v`,
//! `reZ006i` -- each enumerate many thousands of individual code points
//! (`\w`, `\d`, `\i`, and `\c` respectively) in one `instanceDocument`.
//! Embedding every literal character would dwarf every other table in this
//! crate for marginal benefit over a full, lossless run-length compression
//! of the exact same set of code points, so those six are handled
//! separately, in [`mega_groups`] below, against the range tables in
//! `w3c_regex_w3c_data/mega_ranges.rs`.
//!
//! # Baseline
//!
//! Of the resulting 2578 testable cases (2584 single-pattern groups minus
//! the six mega-enumeration groups handled separately) plus those six mega
//! groups: every one of the 2584 `schemaTest`s agrees with this crate
//! (2584/2584, both on whether the pattern compiles and, for the 601 that
//! must not, on the `FORX0002` code it fails with), and all but 25 of the
//! `instanceTest`s already agree with what `Regex::xsd` actually does.
//! Those 25 are [`KNOWN_RESIDUE`], each with a verified, specific reason
//! `Regex::xsd`'s answer is correct and msMeta's is not (or is asking a
//! question outside the pattern facet's scope) -- see the doc comment
//! there and on `crate::charclass::PropName` in `src/charclass.rs`.

use oxixml_regex::{ForxCode, Regex, XsdVersion};

/// One `testGroup`'s `schemaTest`, plus every applicable `instanceTest`.
struct Case {
    /// The XSTS `testGroup`/`schemaTest` name, e.g. `"reA1"` or
    /// `"RegexTest_1"`.
    name: &'static str,
    /// The single `xsd:pattern` facet value.
    pattern: &'static str,
    /// Whether the `schemaTest` expects `pattern` to compile.
    schema_expected_valid: bool,
    /// Every applicable `instanceTest`, evaluated only when
    /// `schema_expected_valid` is `true`.
    instances: &'static [Instance],
}

/// One `instanceTest`: whether the instance document as a whole is
/// expected to validate, and the value(s) extracted from it. XSD validates
/// every element in a document independently and requires *all* of them to
/// validate for the document itself to be valid, so `expected_valid` means
/// "every value in `values` matches" and its negation means "at least one
/// does not" -- not that every value individually fails when the flag is
/// `false` (`reZ006i` is a mega-group example of a document whose name
/// suggests "invalid" values but which validates in full; see
/// [`mega_groups`]).
struct Instance {
    expected_valid: bool,
    values: &'static [&'static str],
}

include!("w3c_regex_w3c_data/cases_part1.rs");
include!("w3c_regex_w3c_data/cases_part2.rs");

/// A `testGroup` name known, and verified, to disagree with `Regex::xsd`'s
/// answer for a specific, documented reason -- never a placeholder for "not
/// investigated yet". Grouped by root cause; see the per-group comment.
///
/// Every entry here also appears in exactly one of the two case-data files
/// and is asserted by [`w3c_regex_w3c_cluster`] to actually reproduce its
/// mismatch, so this list cannot silently drift out of sync with what the
/// crate does: if a fix (in this crate or in `oxixml_unicode`'s Unicode
/// snapshot) ever makes one of these agree with msMeta, or a regression
/// makes some *other* case disagree, the test fails until this list is
/// updated to match reality again.
const KNOWN_RESIDUE: &[(&str, &str)] = &[
    // ------------------------------------------------------------------
    // 16 cases: a `\p{...}` category escape on a Supplementary-Multilingual-
    // Plane code point. msMeta's `<instanceTest>` `<expected
    // validity="invalid"/>` for all 16 contradicts the original test
    // author's own comment in the `.xsd` file (`type='valid'`) and is
    // marked `status="queried"` (not `"accepted"`), citing W3C Bugzilla
    // #4113 (filed by Michael Kay, Saxon's author). That bug's archived
    // text identifies the common thread as "Unicode character classes
    // assigned to characters outside the BMP" -- and every contested code
    // point (e.g. reJ13's U+1D7C9, "MATHEMATICAL SANS-SERIF BOLD ITALIC PI
    // SYMBOL", tested against `\p{Ll}*`) already carried its present-day
    // `General_Category` in Unicode 3.1.0 (March 2001), at or before
    // XSTS's own "circa 2001-2004" authoring window -- checked directly
    // against archived `UnicodeData-3.1.0.txt`/`-4.0.0.txt` snapshots, not
    // assumed. See `crate::charclass::PropName`'s doc comment for the full
    // per-case pattern/code-point breakdown.
    (
        "reJ11",
        "category dispute: \\p{Lu}* on non-BMP U+1D7A8 (W3C bug 4113)",
    ),
    (
        "reJ13",
        "category dispute: \\p{Ll}* on non-BMP U+1D7C9 (W3C bug 4113)",
    ),
    (
        "reJ19",
        "category dispute: \\p{Lo}* on non-BMP U+2FA1D (W3C bug 4113)",
    ),
    (
        "reJ21",
        "category dispute: \\p{M}* on non-BMP combining marks (W3C bug 4113)",
    ),
    (
        "reJ23",
        "category dispute: \\p{Mn}* on non-BMP U+1D1AD (W3C bug 4113)",
    ),
    (
        "reJ25",
        "category dispute: \\p{Mc}* on non-BMP U+1D172 (W3C bug 4113)",
    ),
    (
        "reJ29",
        "category dispute: \\p{N}* on non-BMP number code points (W3C bug 4113)",
    ),
    (
        "reJ31",
        "category dispute: \\p{Nd}* on non-BMP U+1D7FF (W3C bug 4113)",
    ),
    (
        "reJ33",
        "category dispute: \\p{Nl}* on non-BMP U+1034A (W3C bug 4113)",
    ),
    (
        "reJ35",
        "category dispute: \\p{No}* on non-BMP U+10323 (W3C bug 4113)",
    ),
    (
        "reJ61",
        "category dispute: \\p{S}* on non-BMP symbol code points (W3C bug 4113)",
    ),
    (
        "reJ69",
        "category dispute: \\p{So}* on non-BMP U+1D1DD (W3C bug 4113)",
    ),
    (
        "reJ75",
        "category dispute: \\p{Cf}* on non-BMP U+E0078 (W3C bug 4113)",
    ),
    (
        "reJ77",
        "category dispute: (\\p{Co})* on non-BMP private-use planes (W3C bug 4113)",
    ),
    (
        "reS21",
        "category dispute: \\d on non-BMP U+1D7CE (W3C bug 4113)",
    ),
    (
        "reS42",
        "category dispute: \\d on non-BMP U+1D7FF (W3C bug 4113)",
    ),
    // ------------------------------------------------------------------
    // 2 cases: `\D` ("not `\p{Nd}`") correctly matches a code point that is
    // genuinely unassigned (`Cn`) in Unicode 17, exactly as it was in the
    // Unicode 3.1.0/4.0.0 snapshots checked -- there is a real, permanent
    // two-code-point gap in `oxixml_unicode`'s category table between the
    // `Ll` run ending at U+1D7CB and the `Nd` run starting at U+1D7CE, and
    // an unassigned code point is by definition not `Nd`. msMeta's
    // "invalid" (implying these should be digits) has no textual basis in
    // any Unicode version checked, unlike the 16 cases above.
    ("reT63", "\\D correctly matches still-unassigned U+1D7CD"),
    ("reT84", "\\D correctly matches still-unassigned U+1D800"),
    // ------------------------------------------------------------------
    // 7 cases: msMeta's "invalid" comes from a facet or lexical-space rule
    // *other than* the pattern facet under test, so it is outside a single
    // `Regex::xsd` pattern's contract -- confirmed by reading each
    // `.xsd`/`.xml` pair directly, not inferred.
    (
        "reDE3",
        "base='dateTime': \\p{Nd}{4}-\\d\\d-\\d\\dT\\d\\d:\\d\\d:\\d\\d matches '2001-06-06T12:12:61' \
         lexically; second=61 fails xsd:dateTime's own calendar validity, not the pattern",
    ),
    (
        "reDE4",
        "base='dateTime': pattern matches '2001-06-32T12:12:00' lexically; day=32 fails \
         xsd:dateTime's own calendar validity, not the pattern",
    ),
    (
        "reDF3",
        "base='time': \\c+ matches 'abc' lexically; 'abc' is not a valid xsd:time value \
         at all, independent of the pattern",
    ),
    (
        "reDF5",
        "base='time': pattern matches '12345:12:12-12:12' lexically; not a valid xsd:time \
         value, independent of the pattern",
    ),
    (
        "reDH7a",
        "base='xsd:IDREF': \\c[\\c\\d]* matches 'ab' lexically; invalid because no element \
         in the instance document has xsd:ID value 'ab' (a dangling reference), not because \
         of the pattern",
    ),
    (
        "p4",
        "base is xsd:integer restricted by maxExclusive=10 *and* a further pattern; \
         '10' matches [0-9]* but fails the inherited maxExclusive=10 facet",
    ),
    (
        "p7",
        "base is xsd:integer restricted by minInclusive=-9 *and* a further pattern; \
         '-10' matches \\-[0-9]* but fails the inherited minInclusive=-9 facet",
    ),
];

fn residue_reason(name: &str) -> Option<&'static str> {
    KNOWN_RESIDUE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

#[test]
fn w3c_regex_w3c_cluster() {
    // Pins the table's size, not just its content: a regenerated data file
    // that silently dropped rows (exactly the kind of mistake a version-
    // preference or value-extraction bug already produced once while this
    // table was being built -- see the doc comment above) would otherwise
    // pass this test cleanly just by having fewer cases to disagree with.
    assert_eq!(
        CASES_PART1.len() + CASES_PART2.len(),
        2578,
        "case count drifted -- the table should hold exactly the 2584 single-pattern \
         Regex_w3c testGroups minus the six mega-enumeration groups in `mega_groups`"
    );

    let all_cases = CASES_PART1.iter().chain(CASES_PART2.iter());
    let mut unexpected: Vec<String> = Vec::new();
    let mut residue_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for case in all_cases {
        let compiled = Regex::xsd(case.pattern);
        if !case.schema_expected_valid {
            match compiled {
                Ok(_) => unexpected.push(format!(
                    "{}: pattern {:?} compiled but XSTS expects it to be rejected",
                    case.name, case.pattern
                )),
                Err(e) if e.forx_code() != Some(ForxCode::Forx0002) => unexpected.push(format!(
                    "{}: pattern {:?} was rejected with {:?}, expected FORX0002",
                    case.name,
                    case.pattern,
                    e.forx_code()
                )),
                Err(_) => {}
            }
            continue;
        }
        let re = match compiled {
            Ok(re) => re,
            Err(e) => {
                unexpected.push(format!(
                    "{}: pattern {:?} failed to compile ({e}) but XSTS expects it to be valid",
                    case.name, case.pattern
                ));
                continue;
            }
        };
        for inst in case.instances {
            let mut matching = 0;
            let mut evaluation_failed = false;
            for value in inst.values {
                match re.try_is_match(value) {
                    Ok(true) => matching += 1,
                    Ok(false) => {}
                    Err(error) => {
                        evaluation_failed = true;
                        unexpected.push(format!(
                            "{}: pattern {:?}, value {:?}: evaluation failed ({error}); \
                             an execution failure is not a conformance non-match",
                            case.name, case.pattern, value,
                        ));
                    }
                }
            }
            if evaluation_failed {
                // Do not feed partial matching counts into the semantic
                // residue list: resource/cancellation errors are separate.
                continue;
            }
            let all_match = matching == inst.values.len();
            if all_match == inst.expected_valid {
                continue;
            }
            match residue_reason(case.name) {
                Some(_) => {
                    residue_seen.insert(case.name);
                }
                None => unexpected.push(format!(
                    "{}: pattern {:?}, {}/{} value(s) matched, expected all-match={} \
                     (not in KNOWN_RESIDUE)",
                    case.name,
                    case.pattern,
                    matching,
                    inst.values.len(),
                    inst.expected_valid
                )),
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "{} unexpected XSTS Regex_w3c mismatch(es):\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );

    let expected_residue: std::collections::HashSet<&str> =
        KNOWN_RESIDUE.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&str> = expected_residue
        .difference(&residue_seen)
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "KNOWN_RESIDUE entries that no longer reproduce a mismatch (Regex::xsd now agrees \
         with XSTS for these -- update the comment and drop them from the list): {missing:?}"
    );
}

/// Every `Regex_w3c` `schemaTest` whose expected answer depends on which XSD
/// version the pattern is read against: each carries *two* `<expected>`
/// elements, `version="1.0" validity="invalid"` and `version="1.1"
/// validity="valid"`. These sixteen are the complete set — every other
/// `schemaTest` in the testSet carries exactly one, version-less `<expected>`,
/// so [`Case::schema_expected_valid`] doubles as the XSD 1.0 answer for all of
/// them.
///
/// Four distinct patterns are involved. `[^a-d-b-c]` (`reF20`-`reF23`),
/// `[a-c-1-4x-z-7-9]*` (`reG26`-`reG33`), and `[a-a-x-x]+`
/// (`reH19`-`reH21`) each chain a bare `-` between two ranges, which XSD 1.0
/// confines to the first or last member of a character group; `\p{IsaA0-a9}`
/// (`reK88`) names a Unicode block that does not exist, which XSD 1.1 made a
/// match-everything class rather than an error (W3C bug 13670).
const XSD10_ONLY_INVALID: &[&str] = &[
    "reF20", "reF21", "reF22", "reF23", "reG26", "reG27", "reG28", "reG29", "reG30", "reG31",
    "reG32", "reG33", "reH19", "reH20", "reH21", "reK88",
];

/// The [`w3c_regex_w3c_cluster`] sweep re-run under [`XsdVersion::V1_0`],
/// covering only the `schemaTest` half (whether the pattern compiles): the
/// version split is purely syntactic, so no `instanceTest` in the testSet
/// discriminates between the two versions once a pattern has compiled.
///
/// The point of running the *whole* table rather than just the sixteen
/// [`XSD10_ONLY_INVALID`] entries is the negative half: the XSD 1.0 rules are
/// narrower, and this pins that they reject nothing else — in particular that
/// the hyphens XSTS accepts unconditionally still compile, from the trivial
/// `[-]`/`[-a]+`/`[a-]*` (`reH16`-`reH18`) and `[^-z]+` (`reF14`) to
/// `[a-z--[b-z]]` (`reF56`), where a literal `-` is the last group member
/// before a class subtraction.
#[test]
fn w3c_regex_w3c_cluster_under_xsd_1_0() {
    let all_cases = CASES_PART1.iter().chain(CASES_PART2.iter());
    let mut unexpected: Vec<String> = Vec::new();
    let mut split_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for case in all_cases {
        let version_split = XSD10_ONLY_INVALID.contains(&case.name);
        let expected_valid = case.schema_expected_valid && !version_split;
        if version_split {
            assert!(
                case.schema_expected_valid,
                "{}: listed in XSD10_ONLY_INVALID but its XSD 1.1 answer is already `invalid`",
                case.name
            );
            split_seen.insert(case.name);
        }
        match Regex::xsd_versioned(case.pattern, XsdVersion::V1_0) {
            Ok(_) if !expected_valid => unexpected.push(format!(
                "{}: pattern {:?} compiled under XSD 1.0 but XSTS expects it to be rejected",
                case.name, case.pattern
            )),
            Err(e) if expected_valid => unexpected.push(format!(
                "{}: pattern {:?} failed to compile under XSD 1.0 ({e}) but XSTS expects it \
                 to be valid",
                case.name, case.pattern
            )),
            Err(e) if e.forx_code() != Some(ForxCode::Forx0002) => unexpected.push(format!(
                "{}: pattern {:?} was rejected with {:?}, expected FORX0002",
                case.name,
                case.pattern,
                e.forx_code()
            )),
            Ok(_) | Err(_) => {}
        }
    }

    assert!(
        unexpected.is_empty(),
        "{} unexpected XSTS Regex_w3c mismatch(es) under XSD 1.0:\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );

    let expected_split: std::collections::HashSet<&str> =
        XSD10_ONLY_INVALID.iter().copied().collect();
    let missing: Vec<&str> = expected_split.difference(&split_seen).copied().collect();
    assert!(
        missing.is_empty(),
        "XSD10_ONLY_INVALID names that are not in the case table at all: {missing:?}"
    );
}

/// The six `Regex_w3c` testGroups whose `instanceDocument` enumerates many
/// thousands of individual code points against a single-shorthand class
/// (`[\w]`, `[\d]`, `[\i]`, `[\c]`). The exact set of tested code points is
/// preserved losslessly as run-length-compressed `(lo, hi)` ranges in
/// `w3c_regex_w3c_data/mega_ranges.rs` (`compress_ranges` in the
/// (unshipped) generator script that built them just merges adjacent code
/// points; it drops no value XSTS tests and adds none). The expected
/// match/non-match counts below were established once by direct
/// measurement against this crate (matching XSTS's own aggregate
/// document-validity verdict in every case; none of these six is
/// [`KNOWN_RESIDUE`]) and are pinned exactly, not just as a
/// less-than-total or all/any boolean, precisely so that a future
/// regression affecting only a handful of the tens of thousands of code
/// points here cannot hide behind an otherwise-still-"mostly passing"
/// aggregate.
mod mega_groups {
    use oxixml_regex::Regex;

    include!("w3c_regex_w3c_data/mega_ranges.rs");

    fn count_matching(re: &Regex, ranges: &[(u32, u32)]) -> (usize, usize) {
        let mut total = 0usize;
        let mut matching = 0usize;
        for &(lo, hi) in ranges {
            for cp in lo..=hi {
                let Some(c) = char::from_u32(cp) else {
                    continue;
                };
                total += 1;
                match re.try_is_match(&c.to_string()) {
                    Ok(true) => matching += 1,
                    Ok(false) => {}
                    Err(error) => panic!(
                        "Unicode conformance sweep failed at U+{cp:04X}: {error}; \
                         execution errors cannot count as non-matches"
                    ),
                }
            }
        }
        (total, matching)
    }

    /// XSTS `reZ003v`: every one of 9271 code points (run-length-compressed
    /// to 977 ranges) must match `[\w]`.
    #[test]
    fn re_z003v_w_matches_all_9271_code_points() {
        let re = Regex::xsd("[\\w]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ003V);
        assert_eq!(total, 9271);
        assert_eq!(matching, 9271);
    }

    /// XSTS `reZ004v`: of 198 code points (20 ranges), exactly 189 match
    /// `[\d]` under the crate's current Unicode snapshot; 9 do not (the
    /// aggregate document is `<expected validity="invalid"/>`, i.e. XSTS
    /// itself expects at least one non-match here, unlike the other five
    /// mega groups).
    #[test]
    fn re_z004v_d_matches_189_of_198_code_points() {
        let re = Regex::xsd("[\\d]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ004V);
        assert_eq!(total, 198);
        assert_eq!(matching, 189);
    }

    /// XSTS `reZ005v`: every one of 34516 code points (206 ranges) must
    /// match `[\i]`.
    #[test]
    fn re_z005v_i_matches_all_34516_code_points() {
        let re = Regex::xsd("[\\i]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ005V);
        assert_eq!(total, 34516);
        assert_eq!(matching, 34516);
    }

    /// XSTS `reZ005i` ("invalid" list, despite the name -- see the
    /// [`Instance`](super::Instance) doc comment): of the same 34516-point
    /// scale (206 ranges), exactly 34515 match `[\i]`; exactly one does
    /// not, which is enough on its own for the whole `instanceDocument` to
    /// be `<expected validity="invalid"/>`.
    #[test]
    fn re_z005i_i_matches_34515_of_34516_code_points() {
        let re = Regex::xsd("[\\i]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ005I);
        assert_eq!(total, 34516);
        assert_eq!(matching, 34515);
    }

    /// XSTS `reZ006v`: every one of 35122 code points (287 ranges) must
    /// match `[\c]`.
    #[test]
    fn re_z006v_c_matches_all_35122_code_points() {
        let re = Regex::xsd("[\\c]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ006V);
        assert_eq!(total, 35122);
        assert_eq!(matching, 35122);
    }

    /// XSTS `reZ006i` ("invalid" list by name, `invalid.c.xml`): every one
    /// of the same 35122-point scale (288 ranges) matches `[\c]` too. Per
    /// W3C bug 13606, this list is valid using the `NameChar` definitions
    /// of XML 1.1 and XML 1.0 Fifth Edition -- msMeta's *own* applicable
    /// `<expected>` (`version="1.1"`) already says `validity="valid"`,
    /// this crate's XML-name productions match that (see
    /// `oxixml_unicode::xml`), and this test locks that agreement in at
    /// the same code-point granularity as the other five mega groups.
    #[test]
    fn re_z006i_c_matches_all_35122_code_points() {
        let re = Regex::xsd("[\\c]").expect("valid pattern");
        let (total, matching) = count_matching(&re, RANGES_REZ006I);
        assert_eq!(total, 35122);
        assert_eq!(matching, 35122);
    }
}
