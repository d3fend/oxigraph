//! Regression tests for `FORX0002` (invalid pattern) gaps found by auditing
//! every pattern that [QT3's `fn-matches.re` test set][qt3] expects
//! `fn:matches` to reject, against what this crate's XPath dialect
//! ([`Regex::xpath`]) actually did before this pass. All 266 `FORX0002`
//! cases in that test set were checked; 28 were wrongly *accepted*. Each one
//! is reproduced here, grouped by the single underlying defect it exposed
//! (three defects cover all 28 cases):
//!
//! * **Back-references to a group that is not yet closed.** A back-reference
//!   `\N` is valid only once group `N`'s closing `)` has already been parsed,
//!   ruling out both a forward reference (`\1(a)`, where group 1 appears
//!   *after* the reference) and a self-reference (`(\1)`, where the
//!   reference is nested *inside* the very group it names, before that
//!   group's own `)`). See `Parser::parse_backref` in `src/parser.rs`.
//! * **A bare `{` or `}` reaching atom position.** `Char` (XSD Appendix G.3 /
//!   XPath F&O 3.1 §5.6.1) excludes both braces; a literal one must be
//!   written `\{` / `\}`, and an *unescaped* one may only appear as part of a
//!   `{n,m}` quantifier immediately after an atom. See the `'{' | '}'` arm of
//!   `Parser::parse_atom` in `src/parser.rs`.
//! * **Character-class edge cases**: an unescaped `]` outside any class, and
//!   a class-subtraction `-[...]` with nothing before it (`[-[bc]]`). See
//!   `Parser::parse_atom`'s `']'` arm and the `-[` guard in
//!   `Parser::parse_class`, both in `src/parser.rs`.
//!
//! [qt3]: https://github.com/qt4cg/qt3tests (`fn/matches.re.xml`)

use oxixml_regex::{ForxCode, Regex};

/// Asserts every `(name, pattern)` pair is rejected as `FORX0002` under the
/// XPath dialect, reporting the QT3 test-case name on failure.
fn assert_all_forx0002(cases: &[(&str, &str)]) {
    for (name, pattern) in cases {
        let err = Regex::xpath(pattern, "")
            .err()
            .unwrap_or_else(|| panic!("QT3 {name} ({pattern:?}) should be FORX0002 but compiled"));
        assert_eq!(
            err.forx_code(),
            Some(ForxCode::Forx0002),
            "QT3 {name} ({pattern:?}) raised the wrong error: {err}"
        );
    }
}

#[test]
fn backreference_to_a_group_not_yet_closed() {
    assert_all_forx0002(&[
        // \10 forward-references group 10 (there are exactly 10 groups
        // total, so the count check alone would let this through).
        ("re00820", r"\10((((((((((a))))))))))"),
        // \1 forward-references the pattern's only group.
        ("re00821", r"\1(abc)"),
        ("re00822", r"\1([a-c]*)"),
        // \2 forward-references group 2, which is nested *inside* group 1,
        // the group containing the reference.
        ("re00827", r"(\2b*?([a-c]))*"),
        ("re00828", r"(\2b*?([a-c])){3}"),
        // \3 forward-references group 3, the sibling immediately following.
        ("re00829", r"(x(a)\3(\2|b))+"),
        ("re00830", r"((a)\3(\2|b)){2,}"),
        ("re00879", r"\1\d(ab)"),
        ("re00927", r"\1a(\d*){0,2}"),
        ("re00928", r"\1a(\d*){2,}"),
        ("re00929", r"\1a(\d*){0,2}?"),
        ("re00930", r"\1a(\d*){2,}?"),
        ("re00931", r"z\1a(\d*){2,}?"),
        ("re00937", r"((\3|b)\2(a)x)+"),
        ("re00938", r"((\3|b)\2(a)){2,}"),
        // Self-reference: \2 sits inside group 2, before its own ')'.
        ("re00996", r"(.)(\2)"),
        // \11 self-references group 11, the outermost of 11 nested groups.
        ("re00997", r"((((((((((.))))))))))(\11)9"),
    ]);
}

#[test]
fn bare_brace_reaching_atom_position() {
    assert_all_forx0002(&[
        // A complete, well-formed {1} quantity, but with no preceding atom.
        ("re00019", "{1}a"),
        // Unterminated / malformed quantities: '{' never even parses as a
        // quantifier suffix here because nothing precedes it.
        ("re00567", "{5"),
        ("re00568", "{5,"),
        ("re00569", "{5,6"),
        // A second quantifier directly after a piece that already has one:
        // '{0,5}' cannot attach to '*', '+', or '?' — there is no atom left
        // to quantify a second time.
        ("re00686", "^[abcd]*{0,5}$"),
        ("re00687", "^[abcd]+{0,5}$"),
        ("re00688", "^[abcd]?{0,5}$"),
    ]);
}

#[test]
fn character_class_edge_cases() {
    assert_all_forx0002(&[
        // Unescaped ']' outside any class: 'Char' excludes ']'.
        ("re00804", "a]"),
        // '-[' with an empty positive group before it: charClassSub's
        // left-hand posCharGroup can never be empty, so the '-' is read as
        // an ordinary literal member instead, and the unescaped '[' that
        // follows it is then rejected.
        ("re00754", "[^-[bc]]"),
        ("re00988", "[-[xyz]]"),
        ("re00989", "[^-[xyz]]"),
    ]);
}

/// QT3 `re00216`: `\p{IsaA0-a9}` is syntactically a well-formed `IsBlock`
/// name but names no block the engine recognises, so the XPath dialect
/// rejects it. `tests/spec.rs`'s `xpath_unrecognised_block_name_is_invalid`
/// re-checks the same pattern alongside the XSD dialect's contrasting
/// behaviour (XSD 1.1 accepts it — see that test for the full citation), so
/// this is the 28th and last of the QT3-sourced cases, kept here too to keep
/// this file's count matching the audit described above.
#[test]
fn unrecognised_block_name_is_forx0002() {
    assert_all_forx0002(&[("re00216", r"\p{IsaA0-a9}")]);
}
