//! Conformance tests drawn from the XSD Appendix G and the XPath/XQuery
//! *Functions and Operators* §5.6 specifications.

use oxixml_regex::{ForxCode, Limits, Regex, RegexError, XsdVersion};

type R<T = ()> = Result<T, RegexError>;

// ---------------------------------------------------------------------------
// XSD dialect: whole-string anchoring.
// ---------------------------------------------------------------------------

#[test]
fn xsd_is_whole_string_anchored() -> R {
    let re = Regex::xsd("[0-9]{5}(-[0-9]{4})?")?;
    assert!(
        re.try_is_match("12345")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("12345-6789")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("12345-678")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a12345")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("12345 ")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn xsd_alternation_matches_whole_string() -> R {
    let re = Regex::xsd("a|ab")?;
    assert!(
        re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("ab")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("abc")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn xsd_dot_excludes_newline() -> R {
    let re = Regex::xsd(".*")?;
    assert!(
        re.try_is_match("hello")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a\nb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd(".")?
            .try_is_match("\n")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd(".")?
            .try_is_match("\r")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn xsd_caret_and_dollar_are_literals() -> R {
    // In XSD, `^` and `$` are ordinary characters (no anchoring).
    let re = Regex::xsd("^a$")?;
    assert!(
        re.try_is_match("^a$")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    // `\^` is a valid single-character escape; `\$` is not (XPath only).
    assert!(
        Regex::xsd("\\^")?
            .try_is_match("^")
            .expect("regular-expression evaluation must complete")
    );
    assert_eq!(
        Regex::xsd("\\$")
            .expect_err("should be an error")
            .forx_code(),
        Some(ForxCode::Forx0002)
    );
    Ok(())
}

#[test]
fn xsd_quantifiers() -> R {
    assert!(
        Regex::xsd("a?")?
            .try_is_match("")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a?")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a{2,3}")?
            .try_is_match("aa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a{2,3}")?
            .try_is_match("aaa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("a{2,3}")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("a{2,3}")?
            .try_is_match("aaaa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a{2,}")?
            .try_is_match("aaaaa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a{3}")?
            .try_is_match("aaa")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn xsd_single_char_escapes() -> R {
    assert!(
        Regex::xsd("\\t")?
            .try_is_match("\t")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\n")?
            .try_is_match("\n")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("a\\.b")?
            .try_is_match("a.b")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("a\\.b")?
            .try_is_match("axb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\(\\)")?
            .try_is_match("()")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Character-class range subtraction (including nesting).
// ---------------------------------------------------------------------------

#[test]
fn subtraction_class() -> R {
    let re = Regex::xsd("[a-z-[aeiou]]+")?;
    assert!(
        re.try_is_match("rhythm")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("bcd")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("aeiou")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("hello")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn nested_subtraction_class() -> R {
    // [a-z] minus ([a-c] minus [b]) == [a-z] minus {a, c}.
    let re = Regex::xsd("[a-z-[a-c-[b]]]")?;
    assert!(
        re.try_is_match("b")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("d")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("c")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn subtraction_with_case_insensitive_flag() -> R {
    // [A-Z-[IO]] with i matches A, B, a, b but not I, O, i, o.
    let re = Regex::xpath("[A-Z-[IO]]+", "i")?;
    assert!(
        re.try_is_match("AaBb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("I")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("o")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// XML-name-aware escapes \i \c.
// ---------------------------------------------------------------------------

#[test]
fn name_char_escapes() -> R {
    let name = Regex::xsd("\\i\\c*")?;
    assert!(
        name.try_is_match("name")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        name.try_is_match("_id")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        name.try_is_match("ns:local")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        name.try_is_match("a1-b.c")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !name
            .try_is_match("-bad")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !name
            .try_is_match("3abc")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn negated_name_char_escapes() -> R {
    assert!(
        Regex::xsd("\\I")?
            .try_is_match("-")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("\\I")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\C")?
            .try_is_match(" ")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("\\C")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// \p category and block escapes, and \P negation.
// ---------------------------------------------------------------------------

#[test]
fn category_escapes() -> R {
    assert!(
        Regex::xsd("\\p{Lu}+")?
            .try_is_match("ABC")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("\\p{Lu}+")?
            .try_is_match("Abc")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\p{L}+")?
            .try_is_match("abcXYZ")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\p{Nd}")?
            .try_is_match("5")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("\\p{Nd}")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xsd("\\d+")?
            .try_is_match("2026")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn negated_category_escapes() -> R {
    assert!(
        Regex::xsd("\\P{L}")?
            .try_is_match("5")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("\\P{L}")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    // \P{Nd} inside a class.
    assert!(
        Regex::xsd("[\\P{Nd}]")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xsd("[\\P{Nd}]")?
            .try_is_match("5")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn block_escapes() -> R {
    let latin = Regex::xsd("\\p{IsBasicLatin}+")?;
    assert!(
        latin
            .try_is_match("Hello")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !latin
            .try_is_match("h\u{00e9}llo")
            .expect("regular-expression evaluation must complete")
    );
    let supp = Regex::xsd("\\p{IsLatin-1Supplement}")?;
    assert!(
        supp.try_is_match("\u{00e9}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !supp
            .try_is_match("A")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

/// Block escapes over a Basic-Multilingual-Plane-only block are the common
/// case, but `\p{Is...}` names supplementary-plane blocks too, and the
/// underlying membership test (`oxixml_unicode::blocks::in_block`) has to
/// get a code point above `U+FFFF` right, not just fold it into whatever
/// the low 16 bits happen to be. `U+1D100` and `U+1D400` are the first code
/// points of the Musical Symbols and Mathematical Alphanumeric Symbols
/// blocks respectively.
#[test]
fn block_escape_on_a_supplementary_plane_block() -> R {
    let musical = Regex::xsd("\\p{IsMusicalSymbols}+")?;
    assert!(
        musical
            .try_is_match("\u{1D100}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !musical
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );

    let math_alnum = Regex::xsd("\\p{IsMathematicalAlphanumericSymbols}+")?;
    assert!(
        math_alnum
            .try_is_match("\u{1D400}")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !math_alnum
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

/// Every one of the 30 Unicode general categories, and its `\P` negation, on
/// one representative member and one representative non-member. `Cs`
/// (surrogate) is excluded: a Rust `char` is a Unicode scalar value, which
/// by definition excludes surrogate code points, so there is no `char` to
/// construct a member sample from (`surrogate_block_matches_no_char` in
/// `src/charclass.rs` covers that category from the other direction, via a
/// block escape that names a surrogate range).
#[test]
fn every_general_category_and_its_negation() -> R {
    let cases: &[(&str, char, char)] = &[
        ("Lu", 'A', 'a'),
        ("Ll", 'a', 'A'),
        ("Lt", '\u{1C5}', 'a'),  // titlecase digraph Dž
        ("Lm", '\u{2B0}', 'a'),  // MODIFIER LETTER SMALL H
        ("Lo", '\u{3042}', 'a'), // HIRAGANA LETTER A
        ("Mn", '\u{301}', 'a'),  // COMBINING ACUTE ACCENT
        ("Mc", '\u{903}', 'a'),  // DEVANAGARI SIGN VISARGA
        ("Me", '\u{20DD}', 'a'), // COMBINING ENCLOSING CIRCLE
        ("Nd", '5', 'a'),
        ("Nl", '\u{2160}', '5'), // ROMAN NUMERAL ONE
        ("No", '\u{B2}', '5'),   // SUPERSCRIPT TWO
        ("Pc", '_', '-'),
        ("Pd", '-', '_'),
        ("Ps", '(', ')'),
        ("Pe", ')', '('),
        ("Pi", '\u{2018}', '!'), // LEFT SINGLE QUOTATION MARK
        ("Pf", '\u{2019}', '!'), // RIGHT SINGLE QUOTATION MARK
        ("Po", '!', 'a'),
        ("Sm", '+', 'a'),
        ("Sc", '$', 'a'),
        ("Sk", '^', 'a'),
        ("So", '\u{A9}', 'a'), // COPYRIGHT SIGN
        ("Zs", ' ', 'a'),
        ("Zl", '\u{2028}', 'a'),
        ("Zp", '\u{2029}', 'a'),
        ("Cc", '\u{0}', 'a'),
        ("Cf", '\u{AD}', 'a'), // SOFT HYPHEN
        ("Co", '\u{E000}', 'a'),
        ("Cn", '\u{378}', 'a'), // reserved/unassigned in the BMP
    ];
    for (abbrev, member, non_member) in cases {
        let member = member.to_string();
        let non_member = non_member.to_string();
        let pos = Regex::xsd(&format!("\\p{{{abbrev}}}"))?;
        let neg = Regex::xsd(&format!("\\P{{{abbrev}}}"))?;
        assert!(
            pos.try_is_match(&member)
                .expect("regular-expression evaluation must complete"),
            "{abbrev}: {member:?} should match \\p{{{abbrev}}}"
        );
        assert!(
            !neg.try_is_match(&member)
                .expect("regular-expression evaluation must complete"),
            "{abbrev}: {member:?} should not match \\P{{{abbrev}}}"
        );
        assert!(
            !pos.try_is_match(&non_member)
                .expect("regular-expression evaluation must complete"),
            "{abbrev}: {non_member:?} should not match \\p{{{abbrev}}}"
        );
        assert!(
            neg.try_is_match(&non_member)
                .expect("regular-expression evaluation must complete"),
            "{abbrev}: {non_member:?} should match \\P{{{abbrev}}}"
        );
    }
    Ok(())
}

// Corrected from an earlier round: an unrecognised-but-syntactically-shaped
// XSD block name is *not* FORX0002. XSD 1.1 changed this from XSD 1.0 (W3C
// bug 13670; XML Schema Test Suite `msData/regex/reK88`,
// `\p{IsaA0-a9}`, `<expected version="1.1" validity="valid"/>`): "most of
// these strings ... will not denote any Unicode block. Processors may at
// user option treat unrecognized block names as errors" — so a conformant
// XSD 1.1 processor accepts them, and this crate follows the test suite by
// treating them as matching every character (`\P{IsXxx}` on the same name
// then matches none). An *empty* name after `Is` remains invalid
// unconditionally (`reK87`, `\P{Is}`), since it never satisfies the
// `IsBlock ::= 'Is' [a-zA-Z0-9\x2D]+` production at all. The XPath dialect
// does not carry this leniency (confirmed by QT3 `fn-matches.re` `re00216`,
// the same `\p{IsaA0-a9}` pattern, which still expects `FORX0002`), so it
// stays strict for both the empty and the merely-unrecognised case.
#[test]
fn xsd_unrecognised_block_name_matches_everything() -> R {
    let re = Regex::xsd("\\p{IsaA0-a9}")?;
    assert!(
        re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("&")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("\u{10000}")
            .expect("regular-expression evaluation must complete")
    );
    let negated = Regex::xsd("\\P{IsaA0-a9}")?;
    assert!(
        !negated
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

/// The other half of the XSTS `reK88` split: the same `\p{IsaA0-a9}` pattern
/// carries `<expected version="1.0" validity="invalid"/>` next to the
/// `version="1.1" validity="valid"` that
/// [`xsd_unrecognised_block_name_matches_everything`] pins, because the
/// leniency quoted above is a 1.1 change. `\P{IsaA0-a9}` is the same
/// production and is rejected too.
#[test]
fn xsd_1_0_unrecognised_block_name_is_invalid() {
    for pattern in ["\\p{IsaA0-a9}", "\\P{IsaA0-a9}"] {
        let err = Regex::xsd_versioned(pattern, XsdVersion::V1_0)
            .expect_err("reK88: invalid under XSD 1.0");
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    }
    // A block name the engine *does* recognise is unaffected by the version.
    assert!(Regex::xsd_versioned("\\p{IsGreek}+", XsdVersion::V1_0).is_ok());
}

#[test]
fn xsd_empty_block_name_is_invalid() {
    let err = Regex::xsd("\\p{Is}").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    // reK87 carries a single, unconditional `<expected validity="invalid"/>`:
    // an empty name never satisfies `IsBlock` in either version.
    let v1_0 = Regex::xsd_versioned("\\p{Is}", XsdVersion::V1_0).expect_err("should be an error");
    assert_eq!(v1_0.forx_code(), Some(ForxCode::Forx0002));
}

#[test]
fn xpath_unrecognised_block_name_is_invalid() {
    let err = Regex::xpath("\\p{IsaA0-a9}", "").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
    let empty = Regex::xpath("\\p{Is}", "").expect_err("should be an error");
    assert_eq!(empty.forx_code(), Some(ForxCode::Forx0002));
}

// ---------------------------------------------------------------------------
// XPath flags: s, m, i, x, q individually and combined.
// ---------------------------------------------------------------------------

#[test]
fn flag_dot_all() -> R {
    assert!(
        Regex::xpath("a.b", "s")?
            .try_is_match("a\nb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("a.b", "")?
            .try_is_match("a\nb")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn flag_multiline() -> R {
    assert!(
        Regex::xpath("^world$", "m")?
            .try_is_match("hello\nworld")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("^world$", "")?
            .try_is_match("hello\nworld")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("^b", "m")?
            .try_is_match("a\nb")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn flag_case_insensitive() -> R {
    assert!(
        Regex::xpath("hello", "i")?
            .try_is_match("HELLO")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("[a-z]+", "i")?
            .try_is_match("ABC")
            .expect("regular-expression evaluation must complete")
    );
    // Kelvin sign folds to k.
    assert!(
        Regex::xpath("[A-Z]", "i")?
            .try_is_match("\u{212a}")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn flag_ignore_whitespace() -> R {
    assert!(
        Regex::xpath("1 2 3", "x")?
            .try_is_match("123")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("a b c", "x")?
            .try_is_match("abc")
            .expect("regular-expression evaluation must complete")
    );
    // Whitespace inside a class is preserved.
    assert!(
        Regex::xpath("[a b]+", "x")?
            .try_is_match("a b")
            .expect("regular-expression evaluation must complete")
    );
    // A backslash does not protect whitespace from removal: F&O 3.1 §5.6.1
    // states the rule with one exception only, character classes. The
    // backslash therefore binds to the next non-whitespace character, which is
    // what QT3 `K2-MatchesFunc-1` asserts.
    assert!(
        Regex::xpath("hello\\ sworld", "x")?
            .try_is_match("hello world")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("hello\\ sworld", "x")?
            .try_is_match("helloworld")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn flag_literal() -> R {
    assert!(
        Regex::xpath("a.b", "q")?
            .try_is_match("a.b")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("a.b", "q")?
            .try_is_match("axb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("(a)", "q")?
            .try_is_match("(a)")
            .expect("regular-expression evaluation must complete")
    );
    // q combined with i.
    assert!(
        Regex::xpath("A.B", "qi")?
            .try_is_match("a.b")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn flags_combined() -> R {
    // x strips spaces, s makes . match newline.
    assert!(
        Regex::xpath("^ a . b $", "xs")?
            .try_is_match("a\nb")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn invalid_flags() {
    let err = Regex::xpath("a", "z").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0001));
}

// ---------------------------------------------------------------------------
// F&O fn:matches examples.
// ---------------------------------------------------------------------------

#[test]
fn fn_matches_examples() -> R {
    assert!(
        Regex::xpath("bra", "")?
            .try_is_match("abracadabra")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("^a.*a$", "")?
            .try_is_match("abracadabra")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("^bra", "")?
            .try_is_match("abracadabra")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("hello world", "")?
            .try_is_match("hello world")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn end_anchor_without_multiline() -> R {
    assert!(
        Regex::xpath("a$", "")?
            .try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("a$", "")?
            .try_is_match("xa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !Regex::xpath("a$", "")?
            .try_is_match("a\n")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Reluctant (lazy) quantifiers and submatch behaviour.
// ---------------------------------------------------------------------------

#[test]
fn greedy_vs_lazy_submatch() -> R {
    let greedy = Regex::xpath("a(.*)b", "")?;
    assert_eq!(greedy.replace("axxbxxb", "[$1]")?, "[xxbxx]");

    let lazy = Regex::xpath("a(.*?)b", "")?;
    assert_eq!(lazy.replace("axxbxxb", "[$1]")?, "[xx]xxb");
    Ok(())
}

#[test]
fn lazy_replace_spec_example() -> R {
    let lazy = Regex::xpath("a.*?a", "")?;
    assert_eq!(lazy.replace("abracadabra", "*")?, "*c*bra");
    let greedy = Regex::xpath("a.*a", "")?;
    assert_eq!(greedy.replace("abracadabra", "*")?, "*");
    Ok(())
}

// ---------------------------------------------------------------------------
// Back-references.
// ---------------------------------------------------------------------------

#[test]
fn back_reference_simple() -> R {
    let re = Regex::xpath("(a|b)\\1", "")?;
    assert!(
        re.try_is_match("aa")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        re.try_is_match("bb")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("ab")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("ba")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn back_reference_word() -> R {
    let re = Regex::xpath("(\\w+) \\1", "")?;
    assert!(
        re.try_is_match("hello hello")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("hello world")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn back_reference_rejected_in_xsd() {
    let err = Regex::xsd("(a)\\1").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0002));
}

#[test]
fn budget_cutoff_on_catastrophic_backtracking() -> R {
    // A back-reference forces the back-tracking engine; nested quantifiers over
    // a long non-matching input explode, and the budget must cut it off.
    let re = Regex::xpath("(a+)+\\1c", "")?;
    let input: String = std::iter::repeat_n('a', 40).collect();
    let limits = Limits {
        work: 2000,
        ..Limits::default()
    };
    assert!(matches!(
        re.try_is_match_with_limits(&input, limits),
        Err(RegexError::ResourceLimit {
            stage: "work",
            limit: 2000
        })
    ));
    Ok(())
}

#[test]
fn budget_is_cumulative_across_every_start_position_not_reset_per_position() -> R {
    // No single starting position here is expensive: `(z)\1` fails after a
    // handful of steps at every position, since the input never contains a
    // 'z' for the literal to match. The unanchored search tries a new
    // position for every character of the input, though, and an engine that
    // handed each position a fresh budget would let this run to completion
    // (and report a clean `Ok(false)`) no matter how long the input is,
    // because the *total* work is what actually grows without bound, never
    // any single position's share of it. The budget must be spent once
    // across the whole search, not re-granted at every position, so a long
    // enough input still exhausts it.
    let re = Regex::xpath("(z)\\1", "")?;
    let short_input: String = std::iter::repeat_n('a', 1_000).collect();
    let limits = Limits {
        work: 50_000,
        ..Limits::default()
    };
    assert_eq!(re.try_is_match_with_limits(&short_input, limits), Ok(false));

    let long_input: String = std::iter::repeat_n('a', 10_000).collect();
    assert!(matches!(
        re.try_is_match_with_limits(&long_input, limits),
        Err(RegexError::ResourceLimit {
            stage: "work",
            limit: 50_000
        })
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// fn:replace, including the $ numbering rules.
// ---------------------------------------------------------------------------

#[test]
fn replace_basic() -> R {
    assert_eq!(
        Regex::xpath("bra", "")?.replace("abracadabra", "*")?,
        "a*cada*"
    );
    assert_eq!(Regex::xpath("a", "")?.replace("abracadabra", "")?, "brcdbr");
    Ok(())
}

#[test]
fn replace_capture_reference() -> R {
    let re = Regex::xpath("^(.*?)d(.*)$", "")?;
    assert_eq!(re.replace("darted", "$1c$2")?, "carted");
    Ok(())
}

#[test]
fn replace_alternation_capture() -> R {
    let re = Regex::xpath("(ab)|(a)", "")?;
    assert_eq!(re.replace("abcd", "[1=$1][2=$2]")?, "[1=ab][2=]cd");
    Ok(())
}

#[test]
fn replace_whole_match_group_zero() -> R {
    let re = Regex::xpath("a(b)c", "")?;
    assert_eq!(re.replace("abc", "<$0>")?, "<abc>");
    Ok(())
}

#[test]
fn replace_greedy_digit_rule() -> R {
    // Five groups, "$23": 23 > 5 so peel the 3 as a literal, leaving $2.
    let re = Regex::xpath("(a)(b)(c)(d)(e)", "")?;
    assert_eq!(re.replace("abcde", "$23")?, "b3");
    Ok(())
}

#[test]
fn replace_out_of_range_group_is_empty() -> R {
    // One group, "$2": 2 > 1 but <= 9, so the reference is the empty string.
    let re = Regex::xpath("(a)", "")?;
    assert_eq!(re.replace("a", "$2")?, "");
    Ok(())
}

#[test]
fn replace_escapes() -> R {
    let re = Regex::xpath("a", "")?;
    assert_eq!(re.replace("a", "\\$")?, "$");
    assert_eq!(re.replace("a", "\\\\")?, "\\");
    Ok(())
}

#[test]
fn replace_literal_flag_uses_replacement_verbatim() -> R {
    let re = Regex::xpath("a", "q")?;
    assert_eq!(re.replace("a", "$1\\")?, "$1\\");
    Ok(())
}

#[test]
fn replace_invalid_replacement() {
    let re = Regex::xpath("a", "").expect("valid pattern");
    for bad in ["$x", "$", "\\x", "\\", "5$"] {
        let err = re.replace("a", bad).expect_err("should be an error");
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0004), "for {bad:?}");
    }
}

#[test]
fn replace_zero_length_match_is_error() {
    let re = Regex::xpath("a*", "").expect("valid pattern");
    let err = re.replace("xax", "-").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0003));
}

// ---------------------------------------------------------------------------
// fn:tokenize.
// ---------------------------------------------------------------------------

#[test]
fn tokenize_examples() -> R {
    assert_eq!(
        Regex::xpath("\\s+", "")?.tokenize("The cat sat on the mat")?,
        ["The", "cat", "sat", "on", "the", "mat"]
    );
    assert_eq!(
        Regex::xpath("\\s+", "")?.tokenize(" red green blue ")?,
        ["", "red", "green", "blue", ""]
    );
    assert_eq!(
        Regex::xpath(",\\s*", "")?.tokenize("1, 15, 24, 50")?,
        ["1", "15", "24", "50"]
    );
    assert_eq!(
        Regex::xpath(",", "")?.tokenize("1,15,,24,50,")?,
        ["1", "15", "", "24", "50", ""]
    );
    assert_eq!(
        Regex::xpath("(ab)|(a)", "")?.tokenize("abracadabra")?,
        ["", "r", "c", "d", "r", ""]
    );
    assert_eq!(
        Regex::xpath("\\s*<br>\\s*", "i")?.tokenize("Some unparsed <br> HTML <BR> text")?,
        ["Some unparsed", "HTML", "text"]
    );
    Ok(())
}

#[test]
fn tokenize_empty_input() -> R {
    assert_eq!(Regex::xpath(",", "")?.tokenize("")?, Vec::<String>::new());
    Ok(())
}

#[test]
fn tokenize_no_match_returns_whole_input() -> R {
    assert_eq!(Regex::xpath(",", "")?.tokenize("abc")?, ["abc"]);
    Ok(())
}

#[test]
fn tokenize_zero_length_match_is_error() {
    let re = Regex::xpath(".?", "").expect("valid pattern");
    let err = re.tokenize("abba").expect_err("should be an error");
    assert_eq!(err.forx_code(), Some(ForxCode::Forx0003));
}

// ---------------------------------------------------------------------------
// Non-capturing groups and group counting.
// ---------------------------------------------------------------------------

#[test]
fn non_capturing_groups() -> R {
    let re = Regex::xpath("(?:ab)+", "")?;
    assert_eq!(re.group_count(), 0);
    assert!(
        re.try_is_match("ababab")
            .expect("regular-expression evaluation must complete")
    );
    let mixed = Regex::xpath("(a)(?:b)(c)", "")?;
    assert_eq!(mixed.group_count(), 2);
    assert_eq!(mixed.replace("abc", "$1$2")?, "ac");
    Ok(())
}

// ---------------------------------------------------------------------------
// Invalid patterns.
// ---------------------------------------------------------------------------

#[test]
fn invalid_patterns() {
    for pat in ["(", ")", "[z-a]", "a{3,2}", "[]", "a**", "\\q", "[a"] {
        let err = Regex::xpath(pat, "").expect_err("should be an error");
        assert_eq!(err.forx_code(), Some(ForxCode::Forx0002), "for {pat:?}");
    }
}

#[test]
fn error_display_carries_code() {
    let err = Regex::xpath("(", "").expect_err("should be an error");
    assert!(err.to_string().contains("FORX0002"));
}

// ---------------------------------------------------------------------------
// Empty patterns, anchors, and the linear-time (Pike) path.
// ---------------------------------------------------------------------------

#[test]
fn empty_pattern() -> R {
    let re = Regex::xsd("")?;
    assert!(
        re.try_is_match("")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        Regex::xpath("", "")?
            .try_is_match("anything")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn xpath_anchored_whole_string() -> R {
    let re = Regex::xpath("^abc$", "")?;
    assert!(
        re.try_is_match("abc")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("xabcx")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("abc\n")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn nested_quantifier_without_backref_is_linear() -> R {
    // No back-reference, so this runs on the linear-time Pike VM and does not
    // exhibit catastrophic back-tracking even on a long non-matching input.
    let re = Regex::xpath("(a+)+$", "")?;
    let input: String = std::iter::repeat_n('a', 200)
        .chain(std::iter::once('X'))
        .collect();
    assert_eq!(re.try_is_match(&input), Ok(false));
    assert!(
        re.try_is_match(&"a".repeat(200))
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

#[test]
fn counted_repetition_of_group_last_capture_wins() -> R {
    let re = Regex::xpath("(?:(a)|(b)){2}", "")?;
    // Second iteration captures 'b' into group 2, group 1 keeps 'a'.
    assert_eq!(re.replace("ab", "[$1][$2]")?, "[a][b]");
    Ok(())
}

#[test]
fn unicode_input_is_handled_by_codepoint() -> R {
    // An astral-plane code point counts as one character, so "." matches it
    // whole: three code points become three replacements.
    let re = Regex::xpath(".", "")?;
    assert_eq!(re.replace("a\u{1f600}b", "X")?, "XXX");
    // A category escape also sees it as a single code point.
    assert!(
        Regex::xsd("\\p{So}")?
            .try_is_match("\u{1f600}")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Hostile input: pattern compilation must never abort the process.
// ---------------------------------------------------------------------------

/// Regression, through the public API: `Regex::xsd`/`Regex::xpath` used to
/// stack-overflow-abort (SIGABRT, uncatchable) on a deeply parenthesised
/// pattern, because compilation is recursive descent with no depth ceiling.
/// Such a pattern is trivially reachable from an untrusted `xs:pattern` facet
/// or from `fn:matches`/`fn:replace`/`fn:tokenize`, so it must be reported as
/// a typed resource failure, preserving the distinction from invalid syntax.
#[test]
fn deeply_nested_pattern_is_rejected_instead_of_overflowing_the_stack() {
    let depth = 100_000;
    let pattern = format!("{}a{}", "(".repeat(depth), ")".repeat(depth));

    for error in [
        Regex::xsd(&pattern).err(),
        Regex::xpath(&pattern, "").err(),
        // The character-class subtraction production recurses too.
        Regex::xsd(&format!("{}a{}", "[a-".repeat(50_000), "]".repeat(50_000))).err(),
    ] {
        let error = error.expect("a deeply nested pattern must be rejected, not compiled");
        assert!(matches!(error, RegexError::ResourceLimit { .. }), "{error}");
        assert_eq!(error.forx_code(), None, "{error}");
    }
}

/// Realistic nesting depths keep compiling, and matching still works.
#[test]
fn moderately_nested_pattern_still_compiles_and_matches() -> R {
    let depth = 50;
    let pattern = format!("{}ab{}", "(".repeat(depth), ")".repeat(depth));
    let re = Regex::xsd(&pattern)?;
    assert!(
        re.try_is_match("ab")
            .expect("regular-expression evaluation must complete")
    );
    assert!(
        !re.try_is_match("a")
            .expect("regular-expression evaluation must complete")
    );
    Ok(())
}
