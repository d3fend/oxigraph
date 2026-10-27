//! XPath 3.1 §5.6.2 case rules, including inverse and nontransitive variants.
use oxixml_regex::{Regex, RegexError, XsdVersion};

fn matches(input: &str, pattern: &str, flags: &str) -> Result<bool, RegexError> {
    Regex::xpath_versioned(pattern, flags, XsdVersion::V1_1)?.try_is_match(input)
}

#[test]
fn full_case_variants_work_as_literals_classes_ranges_and_backreferences() -> Result<(), RegexError>
{
    for (a, b) in [
        ('k', 'K'),
        ('S', 'ſ'),
        ('Σ', 'ς'),
        ('ß', 'ẞ'),
        ('i', 'ı'),
        ('ﬅ', 'ﬆ'),
        ('θ', 'ϑ'),
        ('θ', 'ϴ'),
    ] {
        for (pattern_char, input_char) in [(a, b), (b, a)] {
            let input = input_char.to_string();
            assert!(matches(&input, &format!("^{pattern_char}$"), "i")?);
            assert!(matches(&input, &format!("^[{pattern_char}]$"), "i")?);
            assert!(matches(
                &input,
                &format!("^[{pattern_char}-{pattern_char}]$"),
                "i"
            )?);
            assert!(matches(
                &format!("{pattern_char}{input_char}"),
                r"^(.)\1$",
                "i"
            )?);
            assert!(!matches(&input, &format!("^{pattern_char}$"), "")?);
        }
    }
    Ok(())
}

#[test]
fn case_matching_does_not_close_transitively_or_fold_whole_strings() -> Result<(), RegexError> {
    // θ shares upper-case Θ with ϑ and lower-case θ with ϴ, but the two
    // endpoints ϑ and ϴ share neither complete mapping.
    for (a, b) in [('ϑ', 'ϴ'), ('İ', 'i'), ('İ', 'ı')] {
        for (pattern_char, input_char) in [(a, b), (b, a)] {
            assert!(!matches(
                &input_char.to_string(),
                &format!("^{pattern_char}$"),
                "i"
            )?);
            assert!(!matches(
                &input_char.to_string(),
                &format!("^[{pattern_char}]$"),
                "i"
            )?);
            assert!(!matches(
                &format!("{pattern_char}{input_char}"),
                r"^(.)\1$",
                "i"
            )?);
        }
    }
    assert!(!matches("ss", "^ß$", "i")?);
    assert!(!matches("ßss", r"^(.)\1$", "i")?);
    assert!(!matches("i\u{307}", "^İ$", "i")?);
    Ok(())
}

#[test]
fn case_rules_preserve_properties_and_apply_before_negation_and_subtraction()
-> Result<(), RegexError> {
    assert!(!matches("a", r"^\p{Lu}$", "i")?);
    assert!(matches("A", r"^\p{Lu}$", "i")?);
    assert!(matches("a", r"^\P{Lu}$", "i")?);
    assert!(!matches("A", r"^\P{Lu}$", "i")?);
    // The Kelvin sign is a variant of K, but lies outside this block.
    assert!(!matches("K", r"^\p{IsBasicLatin}$", "i")?);
    assert!(matches("K", r"^[A-Z]$", "i")?);
    assert!(matches("a", r"^[A-Z-[IO]]$", "i")?);
    for excluded in ["I", "i", "ı", "O", "o"] {
        assert!(!matches(excluded, r"^[A-Z-[IO]]$", "i")?);
    }
    assert!(!matches("K", r"^[^K]$", "i")?);
    assert!(matches("K", r"^\w$", "i")?);
    assert!(!matches("_", r"^\w$", "i")?);
    Ok(())
}

#[test]
fn xml_names_and_unicode_blocks_keep_their_scalar_boundaries() -> Result<(), RegexError> {
    assert!(matches("\u{EFFFF}", r"^\i$", "")?);
    assert!(!matches("\u{F0000}", r"^\i$", "")?);
    assert!(!matches("\u{D7FF}", r"^\p{IsHighSurrogates}$", "")?);
    assert!(matches("\u{D7FF}", r"^\P{IsHighSurrogates}$", "")?);
    assert!(matches("λ", r"^\p{IsGreek}$", "")?);
    assert!(!matches("λ", r"^\p{IsBasicLatin}$", "")?);
    Ok(())
}
