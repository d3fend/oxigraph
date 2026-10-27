//! Regression tests for the XSD-flavour (`Regex::xsd`) corner cases audited
//! against the [XSD Schema Test Suite][xsts] (`xsdtests`), which the ~62
//! pattern-facet failures reported by `oxixml-schema` point back to. Every
//! `pattern value='...'` facet across the whole suite (2577 schemas that use
//! exactly one `xs:pattern`) was checked against `Regex::xsd`; this file
//! reproduces the ones that came out wrong, cross-referenced by XSTS test
//! name (`msData/regex/<name>.xsd` in the suite).
//!
//! [xsts]: https://github.com/w3c/xsdtests

use oxixml_regex::{ForxCode, Regex, XsdVersion};

// ---------------------------------------------------------------------------
// \p{Is...} block-escape legacy aliases (XSD 1.0/Unicode-3.1 spellings).
// ---------------------------------------------------------------------------

#[test]
fn legacy_block_alias_greek() {
    // XSTS reN8: `\p{IsGreek}`. Unicode has since renamed this block "Greek
    // and Coptic"; XSD's block-name table was never updated to match (W3C
    // www-xml-schema-comments bug 5486), so the legacy spelling must keep
    // working.
    let re = Regex::xsd("\\p{IsGreek}+").expect("known legacy block name");
    assert!(
        re.try_is_match("\u{03B1}\u{03B2}\u{03B3}")
            .expect("regular-expression evaluation must complete")
    ); // alpha beta gamma
    assert!(
        !re.try_is_match("abc")
            .expect("regular-expression evaluation must complete")
    );
}

#[test]
fn legacy_block_alias_private_use() {
    // XSTS reL78/reL98/reL99/reM78/reM98/reM99/reN98/reN99: `\p{IsPrivateUse}`,
    // renamed "Private Use Area".
    let re = Regex::xsd("\\p{IsPrivateUse}+").expect("known legacy block name");
    assert!(
        re.try_is_match("\u{E000}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("A")
            .expect("regular-expression evaluation must complete")
    );
}

#[test]
fn legacy_block_alias_combining_marks_for_symbols() {
    // XSTS reL43/reM43/reN43: `\p{IsCombiningMarksforSymbols}`, renamed
    // "Combining Diacritical Marks for Symbols".
    let re = Regex::xsd("\\p{IsCombiningMarksforSymbols}+").expect("known legacy block name");
    assert!(
        re.try_is_match("\u{20D0}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("A")
            .expect("regular-expression evaluation must complete")
    );
}

#[test]
fn legacy_block_alias_cyrillic_supplementary() {
    // Not exercised by XSTS itself (no `msData/regex` test names it), but
    // the same class of drift as the three aliases above: `\p{IsCyrillic
    // Supplementary}` was the Unicode 3.2.0/4.0.0 spelling XSD's block-name
    // table is built from; Unicode 4.0.1 renamed the block "Cyrillic
    // Supplement". Without this alias, `resolve_block_name` would return
    // `None` for the legacy spelling and the XSD dialect would silently
    // fall through to `PropName::UnknownBlock`, matching *every* character
    // instead of just U+0500..=U+052F -- so this checks match/non-match
    // through `Regex::xsd`, not just that the name string resolves.
    let re = Regex::xsd("\\p{IsCyrillicSupplementary}+").expect("known legacy block name");
    assert!(
        re.try_is_match("\u{0500}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("\u{052F}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("A")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("\u{0400}")
            .expect("regular-expression evaluation must complete")
    ); // just outside the block, in plain "Cyrillic"
}

#[test]
fn legacy_block_alias_combined_with_subtraction() {
    // XSTS RegexTest_433: exercises the alias resolution feeding straight
    // into ordinary class-subtraction parsing, with no separate fix needed.
    let re = Regex::xsd("[\\p{IsGreek}-[\\P{Lu}]]+").expect("alias plus subtraction");
    assert!(
        re.try_is_match("\u{0391}")
            .expect("regular-expression evaluation must complete")
    ); // GREEK CAPITAL LETTER ALPHA (Lu)
    assert!(
        !re.try_is_match("\u{03B1}")
            .expect("regular-expression evaluation must complete")
    ); // GREEK SMALL LETTER ALPHA (Ll), subtracted out
}

// ---------------------------------------------------------------------------
// \p{Is...} blocks that fall entirely inside the UTF-16 surrogate range.
// ---------------------------------------------------------------------------

#[test]
fn surrogate_blocks_compile_and_match_nothing() {
    // XSTS reN75 / "HighSurrogates": `\p{IsHighSurrogates}`.
    let high = Regex::xsd("\\p{IsHighSurrogates}+").expect("known block, unrepresentable range");
    // XSTS reM77: `\p{IsLowSurrogates}`.
    let low = Regex::xsd("\\p{IsLowSurrogates}?").expect("known block, unrepresentable range");
    // No `char` can ever be a surrogate code point (Rust's `char` is a
    // Unicode *scalar value*), so both must be incapable of matching
    // anything, over ordinary text and over the empty string alike.
    for sample in ["", "a", "\u{10000}", "\u{FFFF}"] {
        assert!(
            !high
                .try_is_match(sample)
                .expect("regular-expression evaluation must complete"),
            "high surrogates matched {sample:?}"
        );
    }
    assert!(
        low.try_is_match("")
            .expect("regular-expression evaluation must complete")
    ); // `?` — zero occurrences is still a match.
    assert!(
        !low.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
}

// ---------------------------------------------------------------------------
// Degenerate character-class ranges: a bare '-' chained between two ranges.
// XSD 1.1 reads it as an ordinary literal member; XSD 1.0 confines a bare '-'
// to the first or last member of a character group and so rejects all three
// patterns below. Each test pins both directions.
// ---------------------------------------------------------------------------

#[test]
fn degenerate_single_char_range_in_class() {
    // XSTS reH19/reH20/reH21: each carries two `<expected>` elements,
    // `version="1.0" validity="invalid"` and `version="1.1" validity="valid"`.
    // Under 1.1, `[a-a-x-x]` is a class of "a", "-", and "x": two
    // single-character ranges (`a-a`, `x-x`) either side of a literal `-`
    // that does not itself start a second range.
    let re = Regex::xsd("[a-a-x-x]+").expect("valid under XSD 1.1");
    assert!(
        re.try_is_match("a-x")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("xx--aa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("b")
            .expect("regular-expression evaluation must complete")
    );
    assert_eq!(
        Regex::xsd_versioned("[a-a-x-x]+", XsdVersion::V1_0)
            .err()
            .and_then(|e| e.forx_code()),
        Some(ForxCode::Forx0002),
        "reH19-reH21: `<expected version=\"1.0\" validity=\"invalid\"/>`"
    );
}

#[test]
fn multiple_ranges_separated_by_literal_hyphens() {
    // XSTS reG26-reG33: same dual-`<expected>` shape. `[a-c-1-4x-z-7-9]`
    // chains three ranges (a-c, 1-4, x-z... reading greedily) separated by
    // literal '-' members.
    let re = Regex::xsd("[a-c-1-4x-z-7-9]*").expect("valid under XSD 1.1");
    assert!(
        re.try_is_match("a-1x-7")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("")
            .expect("regular-expression evaluation must complete")
    );
    assert_eq!(
        Regex::xsd_versioned("[a-c-1-4x-z-7-9]*", XsdVersion::V1_0)
            .err()
            .and_then(|e| e.forx_code()),
        Some(ForxCode::Forx0002),
        "reG26-reG33: `<expected version=\"1.0\" validity=\"invalid\"/>`"
    );
}

#[test]
fn hyphen_heavy_class_still_parses() {
    // XSTS reF20/reF21/reF22/reF23: same dual-`<expected>` shape as
    // reH19-reH21 above. `[^a-d-b-c]` is "not (a-d, or '-', or b-c)" under
    // 1.1, and no pattern at all under 1.0.
    let re = Regex::xsd("[^a-d-b-c]").expect("valid under XSD 1.1");
    assert!(
        re.try_is_match("z")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );
    assert_eq!(
        Regex::xsd_versioned("[^a-d-b-c]", XsdVersion::V1_0)
            .err()
            .and_then(|e| e.forx_code()),
        Some(ForxCode::Forx0002),
        "reF20-reF23: `<expected version=\"1.0\" validity=\"invalid\"/>`"
    );
}

#[test]
fn hyphen_at_either_end_of_a_group_stays_valid_in_both_versions() {
    // The negative half of the rule above: XSD 1.0 restricts *where* a bare
    // '-' may stand, it does not outlaw it. These five shapes all carry a
    // single, unconditional `<expected validity="valid"/>` in XSTS and so
    // must compile under both versions.
    for (name, pattern) in [
        ("reH16", "[-]"),
        ("reH17", "[-a]+"),
        ("reH18", "[a-]*"),
        ("reF14", "[^-z]+"),
        ("RegexTest_466", "[-e-g]+"),
        // The case that makes "last member" mean "last before the
        // subtraction operator" and not merely "last before `]`": the group
        // is `a-z` plus a literal '-', and the *second* hyphen opens the
        // `-[b-z]` subtraction.
        ("reF56", "[a-z--[b-z]]"),
    ] {
        for version in [XsdVersion::V1_0, XsdVersion::V1_1] {
            assert!(
                Regex::xsd_versioned(pattern, version).is_ok(),
                "{name}: {pattern:?} must compile under {version:?}"
            );
        }
    }
}

#[test]
fn class_range_chaining_follows_the_xpath_dialects_xsd_version_too() {
    // The three patterns above (`[^a-d-b-c]`, `[a-c-1-4x-z-7-9]*`,
    // `[a-a-x-x]+`) are a genuine XSD-version split, not a shape one dialect
    // may reject outright -- checked mechanically against both reference
    // suites, not assumed:
    //
    // * XSTS msMeta's `<schemaTest>` for reF20-23/reG26-33/reH19-21 all
    //   carry *both* `<expected version="1.0" validity="invalid"/>` *and*
    //   `<expected version="1.1" validity="valid"/>` -- XSD 1.1's errata
    //   (`RULE='14'`/`'15,16'`/`'22'` in the test annotations) relaxed
    //   exactly this rule: a `-` that cannot attach to a range falls back
    //   to a literal member instead of being a hard parse error.
    // * QT3 `fn-matches.re` encodes the identical split as sibling test
    //   cases carrying `<dependency type="xsd-version" value="..."/>`:
    //   `re00056` (`xsd-version=1.0`) expects `FORX0002` for
    //   `[^a-d-b-c]`, while `re00056a` (byte-for-byte the same pattern,
    //   `xsd-version=1.1`) expects success. Likewise `re00086`/`re00086a`
    //   for `[a-c-1-4x-z-7-9]*` and `re00102a`/`re00102` for `[a-a-x-x]+`.
    //   The XSLT 3.0 catalogue splits its own `regex-syntax-0056`/`-0086`/
    //   `-0102` the same way, on `<feature value="XSD_1.1" satisfied="..."/>`.
    //
    // So the *dialect* does not decide it: the XSD version the processor
    // implements does, in the XPath dialect exactly as in the `pattern`-facet
    // one. `Regex::xpath` names the version this workspace reports
    // (`system-property('xsl:xsd-version')` = `1.0`), and
    // `Regex::xpath_versioned` names the other. Both directions are pinned
    // here, with QT3's own value lists.
    for pattern in [
        "^(?:[^a-d-b-c])$",
        "^(?:[a-c-1-4x-z-7-9]*)$",
        "^(?:[a-a-x-x]+)$",
    ] {
        assert_eq!(
            Regex::xpath(pattern, "").err().and_then(|e| e.forx_code()),
            Some(ForxCode::Forx0002),
            "{pattern:?} is FORX0002 for an XSD 1.0 processor"
        );
        assert_eq!(
            Regex::xpath_versioned(pattern, "", XsdVersion::V1_0)
                .err()
                .and_then(|e| e.forx_code()),
            Some(ForxCode::Forx0002),
            "{pattern:?} is FORX0002 under an explicit XSD 1.0"
        );
    }

    let f = Regex::xpath_versioned("^(?:[^a-d-b-c])$", "", XsdVersion::V1_1)
        .expect("valid under XSD 1.1");
    assert!(
        f.try_is_match("z")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !f.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !f.try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );

    let g = Regex::xpath_versioned("^(?:[a-c-1-4x-z-7-9]*)$", "", XsdVersion::V1_1)
        .expect("valid under XSD 1.1");
    for s in [
        "",
        "a-1x-7",
        "c-4z-9",
        "a-1z-8a-1z-9",
        "a1z-9",
        "a-1z8",
        "a-1",
        "z-9",
    ] {
        assert!(
            g.try_is_match(s)
                .expect("regular-expression evaluation must complete"),
            "{s:?} should match (QT3 re00086a)"
        );
    }

    let h = Regex::xpath_versioned("^(?:[a-a-x-x]+)$", "", XsdVersion::V1_1)
        .expect("valid under XSD 1.1");
    assert!(
        h.try_is_match("a-x")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !h.try_is_match("j")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !h.try_is_match("a-b")
            .expect("regular-expression evaluation must complete")
    );
}

/// The same split, on the three further patterns a mechanical sweep of both catalogues turned
/// up: every pattern string in `qt3tests` and `xslt30-test` (15 417 distinct ones — every
/// `regex="..."` attribute, every `<param name="regex">`, and every quoted literal in the
/// `matches`/`replace`/`tokenize`/`analyze-string` files) was compiled under both versions, and
/// the *only* patterns whose acceptance differs are the three above and these three. Each is
/// gated on the XSD version by the catalogue itself, and each must be `FORX0002` for a processor
/// that reports XSD 1.0:
///
/// * `[0-9-.]*/` — QT3 `fn/matches` `K2-MatchesFunc-16a` (`<dependency type="xsd-version"
///   value="1.0"/>`, `<error code="FORX0002"/>`) against `-16` (`value="1.1"`, answers `false`).
/// * `([\d-z]+)` and `([\d-\s]+)` — QT3 `fn/matches/perl-tests.xml` `p888`-`p891`, driven by
///   `fn-matches-50`, which carries `<dependency type="xsd-version" value="1.0"/>` and the
///   comment *"four of the subtests assume XSD 1.0 rules on hyphens"*. That case's own logic
///   accepts a raised error for a subtest whose `@result` is neither `y` nor `n`, and reports a
///   `<fail>` for a *successful* match — so the lenient reading is what would fail it.
#[test]
fn the_hyphen_rule_covers_every_further_pattern_the_catalogues_gate_on_the_version() {
    for pattern in ["[0-9-.]*/", r"([\d-z]+)", r"([\d-\s]+)"] {
        assert_eq!(
            Regex::xpath(pattern, "").err().and_then(|e| e.forx_code()),
            Some(ForxCode::Forx0002),
            "{pattern:?} is FORX0002 for an XSD 1.0 processor"
        );
        assert!(
            Regex::xpath_versioned(pattern, "", XsdVersion::V1_1).is_ok(),
            "{pattern:?} compiles under XSD 1.1"
        );
    }
}

/// The negative half, in the XPath dialect: XSD 1.0 restricts *where* a bare
/// `-` may stand, it does not outlaw it, and it changes nothing outside a
/// character class. Every one of these carries a single unconditional
/// `<expected validity="valid"/>` in XSTS or an unconditional pass in QT3, so
/// none of them may move when the XPath dialect starts reading XSD 1.0.
#[test]
fn the_xpath_dialect_keeps_every_hyphen_both_versions_allow() {
    for pattern in [
        "[-]",
        "[-a]+",
        "[a-]*",
        "[^-z]+",
        "[-e-g]+",
        "[a-z--[b-z]]",
        "[a-z-[aeiou]]+",
        "[A-Z-[IO]]+",
        "[^a-z-[c-e]]",
        r"[\n\r\t\\\|\.\-\^\?\*\+\{\}\[\]\(\)]*",
        r"[a-d\--[bc]]",
        // Outside a class a hyphen is an ordinary character in both versions.
        "a-b-c",
        "-",
    ] {
        assert!(
            Regex::xpath(pattern, "").is_ok(),
            "{pattern:?} must still compile for an XSD 1.0 processor"
        );
    }
}

// ---------------------------------------------------------------------------
// A raw (unescaped) '-' may never be a range *endpoint*, on either side,
// distinct from the chaining rule confirmed just above. This is the mirror
// image of that rule: a lone `-` that cannot attach to a range falls back to
// a literal member (never an error); but a `-` that a lookahead has already
// committed to treating as a range operator or endpoint, and which turns out
// to itself be a second, unescaped `-`, is a hard parse error in both
// dialects, unconditionally (no XSD-version split).
// ---------------------------------------------------------------------------

#[test]
fn adjacent_raw_hyphens_may_not_form_a_range() {
    // XSTS `saxonData/Simple/simple041.bad.xsd`: `[--z]*`. The file's own
    // ".bad.xsd" name (contrasted with `simple043.xsd`, literally renamed
    // *away* from ".bad" once its own hyphen pattern -- `[+-]*` -- was ruled
    // valid by the WG in Nov 2010 per bug 13922) and its unrevised
    // `<current status="accepted" date="2010-10-11"/>` with no `<prior>`
    // split (unlike simple043's, which *was* revised) both signal this test
    // was never swept up in that relaxation and stays invalid under 1.1.
    // Confirmed: `<schemaTest>` carries a single, unconditional
    // `<expected validity="invalid"/>`.
    //
    // Per XSD Appendix G.3, `seRange ::= charOrEsc '-' charOrEsc` (rule 16)
    // and `charOrEsc ::= XmlChar | SingleCharEsc` (rule 17), where `XmlChar`
    // excludes '-', '[', and ']' (rule 18). A raw hyphen therefore never
    // satisfies `charOrEsc`, so it can never be the *start* of a range:
    // `[--z]`'s first '-' cannot open `'-' '-' charOrEsc('z')`, because the
    // grammar has no production under which a bare '-' begins a `seRange`.
    let simple041 = Regex::xsd("[--z]*")
        .expect_err("simple041: [--z]* must be invalid (XSTS <expected validity=\"invalid\"/>)");
    assert_eq!(simple041.forx_code(), Some(ForxCode::Forx0002));

    // XSTS `saxonData/Simple/simple042.bad.xsd`: `[!--]*`, same ".bad.xsd" /
    // unrevised-metadata signal as simple041, single unconditional
    // `<expected validity="invalid"/>`. Here the *second* '-' (immediately
    // after the range-operator '-' that follows '!') is the one that fails
    // `charOrEsc`: `'!' '-' '-'` cannot complete `charOrEsc('!') '-'
    // charOrEsc(???)` because a bare '-' isn't a valid `charOrEsc` end either.
    let simple042 = Regex::xsd("[!--]*")
        .expect_err("simple042: [!--]* must be invalid (XSTS <expected validity=\"invalid\"/>)");
    assert_eq!(simple042.forx_code(), Some(ForxCode::Forx0002));

    // QT3 `fn-matches.re` `re00071`: `matches('qwerty','[a--b]')`, an
    // *unconditional* `<error code="FORX0002"/>` with no
    // `<dependency type="xsd-version">` and no `<any-of>` -- i.e. this is not
    // an XSD-1.0-vs-1.1 split like the chaining cases above, it is invalid in
    // both dialects and both XSD versions. The XML Schema Test Suite carries
    // the identical pattern independently as `msData/regex/reF57.xsd`
    // (`RULE='15,16'`, i.e. the same `charRange`/`seRange` productions),
    // also with a single unconditional `<expected validity="invalid"/>` --
    // two independent suites agreeing this exact shape is invalid confirms
    // it is not a suite quirk. Checked in the XPath dialect (QT3's own),
    // matching the literal value used in `matches('qwerty', ...)`.
    let re00071 = Regex::xpath("[a--b]", "")
        .expect_err("re00071/reF57: [a--b] must be invalid in both dialects");
    assert_eq!(re00071.forx_code(), Some(ForxCode::Forx0002));
    // Also invalid in the XSD dialect (the two dialects share one grammar
    // for character classes; `reF57.xsd` exercises exactly this).
    assert_eq!(
        Regex::xsd("[a--b]").err().and_then(|e| e.forx_code()),
        Some(ForxCode::Forx0002)
    );
}

#[test]
fn adjacent_hyphens_before_subtraction_are_unaffected() {
    // XSTS `msData/regex/reF56.xsd`: `[a-z--[b-z]]`, `RULE='15,16'` (the same
    // annotation as reF57 immediately above), but `<expected
    // validity="valid"/>` -- the crucial difference from `[a--b]` is that the
    // second '-' here is immediately followed by '[', so it is never a range
    // attempt in the first place: it opens a class subtraction
    // (`charClassSub ::= posCharGroup '-' charClassExpr`, rule 14) on the
    // preceding `posCharGroup` ("a-z" then the literal '-' via
    // `XmlCharIncDash`). `[a-z] + {'-'}` minus `[b-z]` leaves just `{a, '-'}`.
    let re = Regex::xsd("[a-z--[b-z]]").expect("reF56: valid, adjacent '-' starts a subtraction");
    assert!(
        re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("b")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("z")
            .expect("regular-expression evaluation must complete")
    );
}

#[test]
fn escaped_hyphen_is_a_legal_range_endpoint() {
    // The mirror-image positive case: `charOrEsc ::= XmlChar | SingleCharEsc`
    // (rule 17) excludes a *raw* '-' but the escape `\-` satisfies
    // `SingleCharEsc`, so it legally names U+002D as either endpoint of a
    // `seRange` -- unlike the raw '-' rejected by
    // `adjacent_raw_hyphens_may_not_form_a_range` above. `[\--z]` names
    // exactly the range U+002D..='z'.
    let start = Regex::xsd("[\\--z]*").expect("escaped start must be a legal range endpoint");
    assert!(
        start
            .try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        start
            .try_is_match("A")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        start
            .try_is_match("z")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !start
            .try_is_match(",")
            .expect("regular-expression evaluation must complete"),
        "U+002C is just below the U+002D start"
    );
    assert!(
        !start
            .try_is_match("{")
            .expect("regular-expression evaluation must complete"),
        "'{{' is just past 'z'"
    );

    // `[!-\-]` names the range '!'..=U+002D.
    let end = Regex::xsd("[!-\\-]*").expect("escaped end must be a legal range endpoint");
    assert!(
        end.try_is_match("!")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        end.try_is_match("#")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        end.try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !end.try_is_match(" ")
            .expect("regular-expression evaluation must complete"),
        "space U+0020 is just below '!'"
    );
    assert!(
        !end.try_is_match(".")
            .expect("regular-expression evaluation must complete"),
        "'.' U+002E is just past the U+002D end"
    );
}

// ---------------------------------------------------------------------------
// \p{Ll} / \p{M} category-membership audit: the mapping in
// `src/charclass.rs` is unchanged, verified correct as-is. See the doc
// comment on `charclass::PropName` for the full evidence trail.
// ---------------------------------------------------------------------------

#[test]
fn category_escapes_use_current_unicode_not_stale_xsts_metadata() {
    // XSTS reJ13 (`\p{Ll}*` against U+0061, U+1D7C9) and reJ21 (`\p{M}*`
    // against a nine-code-point run of combining marks) both carry a
    // `<instanceTest>` `<expected validity="invalid"/>` in msMeta, i.e. the
    // suite asserts these values should *not* fully match. This crate
    // deliberately disagrees, because every code point involved really is
    // `Ll` / `Mn`/`Mc`/`Me` in Unicode 17 (`oxixml_unicode::UNICODE_VERSION`)
    // -- confirmed directly against `oxixml_unicode::gc::general_category`,
    // not just this crate's own logic.
    //
    // That "invalid" is not settled ground truth: it *contradicts* the
    // original test author's own comment in reJ13.xsd/reJ21.xsd
    // (`type='valid'`), is marked `status="queried"` (not `"accepted"`) in
    // msMeta, and cites W3C Bugzilla #4113 (filed by Michael Kay -- Saxon's
    // author and the editor of the QT3 `fn-matches.re` catalogue this crate
    // is also graded against), whose archived text identifies the common
    // thread across this and its siblings as "Unicode character classes
    // assigned to characters outside the BMP" -- not an assignment-timing
    // problem: U+1D7C9 and every code point in `nine_marks` already carried
    // its present-day category in Unicode 3.1.0 (March 2001), at or before
    // XSTS's own "circa 2001-2004" authoring window (checked against the
    // vendored `UnicodeData-3.1.0.txt`). The bug was never resolved before
    // the old W3C Bugzilla was archived in 2019; see the full evidence
    // trail on `charclass::PropName`'s doc comment and
    // `tests/w3c_regex_w3c.rs`, which runs this same audit over the whole
    // `Regex_w3c` cluster rather than just these two testGroups.
    let ll = Regex::xsd("\\p{Ll}*").expect("valid pattern");
    assert!(
        ll.try_is_match("a\u{1D7C9}")
            .expect("regular-expression evaluation must complete"),
        "U+1D7C9 is Ll in Unicode 17 (XSTS reJ13; W3C bug 4113)"
    );

    let m = Regex::xsd("\\p{M}*").expect("valid pattern");
    let nine_marks = "\u{64B}\u{1D1AD}\u{903}\u{1D172}\u{903}\u{1D172}\u{20DD}\u{20DD}\u{20E0}";
    assert!(
        m.try_is_match(nine_marks)
            .expect("regular-expression evaluation must complete"),
        "every code point here is Mn/Mc/Me in Unicode 17 (XSTS reJ21; W3C bug 4113)"
    );

    // Paired negative control from the same two testGroups (reJ14 / reJ22,
    // both plainly "accepted" in msMeta -- not disputed): U+01C5 is `Lt`
    // (titlecase), not `Ll` or `M`, and both patterns correctly reject it,
    // both positively and through `\P`.
    assert!(
        !ll.try_is_match("\u{1C5}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !m.try_is_match("\u{1C5}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\P{Ll}")
            .expect("valid pattern")
            .try_is_match("\u{1C5}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\P{M}")
            .expect("valid pattern")
            .try_is_match("\u{1C5}")
            .expect("regular-expression evaluation must complete")
    );
}

// ---------------------------------------------------------------------------
// Universal fixes also confirmed by the XSD Schema Test Suite (the same
// underlying defects as tests/qt3_forx0002.rs, since these patterns are
// invalid in *both* dialects — see `Parser::parse_atom` / `parse_class`).
// ---------------------------------------------------------------------------

#[test]
fn bare_brace_is_invalid_in_xsd_too() {
    // XSTS reB65, RegexTest_24, RegexTest_25, RegexTest_26: all
    // `<expected validity="invalid"/>` unconditionally (no XSD-version
    // split), contradicting the once-common assumption that XSD treats an
    // unparseable '{' leniently as a literal character.
    for (name, pattern) in [
        ("reB65", "{1}a"),
        ("RegexTest_24", "{5"),
        ("RegexTest_25", "{5,"),
        ("RegexTest_26", "{5,6"),
    ] {
        let err = Regex::xsd(pattern)
            .err()
            .unwrap_or_else(|| panic!("XSTS {name} ({pattern:?}) should be invalid"));
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    }
}

#[test]
fn double_quantifier_is_invalid_in_xsd_too() {
    // XSTS RegexTest_311/312/313, annotated "bogus double exponent in a
    // pattern": a second quantifier directly after one that already applies.
    for (name, pattern) in [
        ("RegexTest_311", "^[abcd]*{0,5}$"),
        ("RegexTest_312", "^[abcd]+{0,5}$"),
        ("RegexTest_313", "^[abcd]?{0,5}$"),
    ] {
        let err = Regex::xsd(pattern)
            .err()
            .unwrap_or_else(|| panic!("XSTS {name} ({pattern:?}) should be invalid"));
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    }
}

#[test]
fn empty_group_before_subtraction_is_invalid_in_xsd_too() {
    // XSTS RegexTest_469/RegexTest_470: `[^-[bc]]`.
    for (name, pattern) in [("RegexTest_469", "[^-[bc]]"), ("RegexTest_470", "[^-[bc]]")] {
        let err = Regex::xsd(pattern)
            .err()
            .unwrap_or_else(|| panic!("XSTS {name} ({pattern:?}) should be invalid"));
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    }
}

#[test]
fn bare_close_bracket_is_invalid_in_xsd_too() {
    // XSTS RegexTest_586/755/1070/1239: `a]`.
    for name in [
        "RegexTest_586",
        "RegexTest_755",
        "RegexTest_1070",
        "RegexTest_1239",
    ] {
        let err = Regex::xsd("a]")
            .err()
            .unwrap_or_else(|| panic!("XSTS {name} (\"a]\") should be invalid"));
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    }
}

// ---------------------------------------------------------------------------
// replace/tokenize honour the same strictness: they operate on an
// already-compiled `Regex`, so an invalid pattern never reaches them at all —
// parsing (shared by every entry point) is where the rejection happens.
// ---------------------------------------------------------------------------

#[test]
fn replace_and_tokenize_share_the_same_parser() {
    for pattern in ["{1}a", "a]", "[^-[bc]]"] {
        assert_eq!(
            Regex::xsd(pattern).err().and_then(|e| e.forx_code()),
            Some(ForxCode::Forx0002),
            "XSD construction should reject {pattern:?} before replace/tokenize are reachable"
        );
        assert_eq!(
            Regex::xpath(pattern, "").err().and_then(|e| e.forx_code()),
            Some(ForxCode::Forx0002),
            "XPath construction should reject {pattern:?} before replace/tokenize are reachable"
        );
    }
}
