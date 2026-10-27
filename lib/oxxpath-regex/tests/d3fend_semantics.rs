use oxixml_regex::{Limits, Regex, RegexError, XsdVersion};
use std::cell::Cell;

fn xpath(pattern: &str, flags: &str) -> Regex {
    Regex::xpath_versioned(pattern, flags, XsdVersion::V1_1)
        .expect("small XPath semantic fixture must compile within default limits")
}

#[test]
fn xpath_backreferences_classes_and_flags() {
    for (pattern, flags, input, expected) in [
        (
            r"^(a)\10(b)(c)(d)(e)(f)(g)(h)(i)(j)$",
            "",
            "aa0bcdefghij",
            true,
        ),
        (r"^(a)?b\1$", "", "b", true),
        (r"^[a-z-[aeiou]]+$", "", "bcdf", true),
        (r"^[a-z-[aeiou]]+$", "", "bad", false),
        (r"^\i\c*$", "", "name:with-dash", true),
        (r"\p{IsBasicLatin}", "", "a", true),
        ("\n$", "m", "\n", false),
        ("^b$", "m", "a\nb\nc", true),
        ("a.b", "", "a\rb", false),
        ("a.b", "s", "a\rb", true),
        ("a b", "x", "ab", true),
        ("a b", "qx", "a b", true),
        ("k", "i", "K", true),
        ("ﬅ", "i", "ﬆ", true),
    ] {
        assert_eq!(
            xpath(pattern, flags).try_is_match(input),
            Ok(expected),
            "{pattern:?}, {flags:?}, {input:?}"
        );
    }
}

#[test]
fn bounded_repeats_fail_explicitly_and_optional_compile_is_iterative() {
    assert!(matches!(
        Regex::xpath_versioned("a{200001}|b{200002}", "", XsdVersion::V1_1),
        Err(RegexError::ResourceLimit { .. })
    ));
    // This used to recurse once for every optional copy during compilation.
    let regex = xpath("a{0,1000}", "");
    assert_eq!(regex.try_is_match("a"), Ok(true));
}

#[test]
fn all_matches_and_replacement_share_one_work_allowance() {
    let regex = xpath("a", "");
    let input = "a".repeat(200);
    // Admission and initial searches fit; repeated searches must not each reset it.
    let limits = Limits {
        work: 2_000,
        ..Limits::default()
    };
    let mut matches = regex.find_iter_with_limits(&input, limits);
    assert!(
        matches
            .next()
            .expect("input must produce an initial match")
            .is_ok()
    );
    assert!(
        matches.any(|item| matches!(item, Err(RegexError::ResourceLimit { stage: "work", .. })))
    );
    assert!(matches!(
        regex.replace_with_limits(&input, "a", limits),
        Err(RegexError::ResourceLimit { stage: "work", .. })
    ));
}

#[test]
fn input_output_capture_and_allocation_limits_are_typed() {
    let regex = xpath("(a)", "");
    assert!(matches!(
        Regex::xpath_with_limits(
            "(a)(b)",
            "",
            XsdVersion::V1_1,
            Limits {
                captures: 1,
                ..Limits::default()
            }
        ),
        Err(RegexError::ResourceLimit {
            stage: "captures",
            ..
        })
    ));
    assert!(matches!(
        regex.try_is_match_with_limits(
            "aa",
            Limits {
                input_bytes: 1,
                ..Limits::default()
            }
        ),
        Err(RegexError::ResourceLimit {
            stage: "input bytes",
            ..
        })
    ));
    assert!(matches!(
        regex.try_is_match_with_limits(
            "a",
            Limits {
                allocation_bytes: 1,
                ..Limits::default()
            }
        ),
        Err(RegexError::ResourceLimit { .. })
    ));
    assert!(matches!(
        regex.replace_with_limits(
            "aa",
            "abc",
            Limits {
                output_bytes: 5,
                ..Limits::default()
            }
        ),
        Err(RegexError::ResourceLimit {
            stage: "output bytes",
            ..
        })
    ));
}

#[test]
fn cancellation_is_typed_during_compilation_matching_and_replacement() {
    assert!(matches!(
        Regex::xpath_with_cancel("a", "", XsdVersion::V1_1, Limits::default(), &|| true),
        Err(RegexError::Cancelled)
    ));
    let regex = xpath("(a+)\\1", "");
    assert_eq!(
        regex.try_is_match_with_cancel("aaaa", Limits::default(), &|| true),
        Err(RegexError::Cancelled)
    );
    assert_eq!(
        regex.replace_with_cancel("aaaa", "x", Limits::default(), &|| true),
        Err(RegexError::Cancelled)
    );
    let calls = Cell::new(0usize);
    let cancelled = || {
        let n = calls.get();
        calls.set(n + 1);
        n >= 12
    };
    assert_eq!(
        xpath("a", "").replace_with_cancel(&"a".repeat(100), "b", Limits::default(), &cancelled),
        Err(RegexError::Cancelled)
    );
    assert!(calls.get() > 12);
}

#[test]
fn replacement_capture_prefixes_and_literal_quoting() {
    assert_eq!(
        xpath("(a)", "").replace("a", "$10-$01-$9-$0"),
        Ok("a0-a--a".into())
    );
    assert_eq!(xpath("a", "q").replace("aa", "$1\\"), Ok("$1\\$1\\".into()));
    assert!(matches!(
        xpath("a", "").replace("z", "$x"),
        Err(RegexError::InvalidReplacement(_))
    ));
}

#[test]
fn backtracking_retries_branches_quantifiers_and_restores_captures() {
    for (pattern, input, expected) in [
        (r"^(a|ab)\1$", "abab", true),
        (r"^(a*)a\1$", "aaaaa", true),
        (r"^(a*)a\1$", "aaaa", false),
        (r"^(a*?)a\1$", "aaaaa", true),
        (r"^((a)b|a)\2$", "a", true),
    ] {
        assert_eq!(
            xpath(pattern, "").try_is_match(input),
            Ok(expected),
            "pattern={pattern:?}, input={input:?}"
        );
    }
}

#[test]
fn replacement_uses_first_alternative_and_checks_nullable_backreferences() {
    assert_eq!(
        xpath("(a)|(ab)", "").replace("ab", "[$1][$2]"),
        Ok("[a][]b".into())
    );
    assert_eq!(
        xpath(r"(a)?\1", "").replace("z", "X"),
        Err(RegexError::MatchesEmptyString)
    );
}

#[test]
fn repeated_flags_are_valid_but_literal_mode_still_rejects_unknown_flags() {
    assert_eq!(xpath("^a$", "iimmsx").try_is_match("A"), Ok(true));
    assert!(matches!(
        Regex::xpath_versioned("a", "qz", XsdVersion::V1_1),
        Err(RegexError::InvalidFlags(_))
    ));
}
