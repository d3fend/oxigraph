//! Tests for the public per-match, per-capturing-group API:
//! [`Regex::find_iter`], [`Regex::find`], [`Match`], and [`Matches`].

use oxixml_regex::{Limits, Match, Regex, RegexError};

type R<T = ()> = Result<T, RegexError>;

#[test]
fn finds_every_non_overlapping_match() -> R {
    let re = Regex::xpath("[0-9]+", "")?;
    let matches: Vec<_> = re.find_iter("a1b22c333").collect::<Result<_, _>>()?;
    let texts: Vec<&str> = matches.iter().map(|m| m.as_str()).collect();
    assert_eq!(texts, ["1", "22", "333"]);
    Ok(())
}

#[test]
fn no_match_yields_an_empty_iterator() -> R {
    let re = Regex::xpath("[0-9]+", "")?;
    let matches: Vec<_> = re.find_iter("abc").collect::<Result<_, _>>()?;
    assert!(matches.is_empty());
    Ok(())
}

#[test]
fn match_range_and_accessors() -> R {
    let re = Regex::xpath("[0-9]+", "")?;
    let m = re.find("abc123def")?.expect("should match");
    assert_eq!(m.range(), 3..6);
    assert_eq!(m.start(), 3);
    assert_eq!(m.end(), 6);
    assert_eq!(m.as_str(), "123");
    assert!(!m.is_empty());
    Ok(())
}

#[test]
fn find_returns_none_without_a_match() -> R {
    let re = Regex::xpath("[0-9]+", "")?;
    assert_eq!(re.find("no digits here")?, None);
    Ok(())
}

// ---------------------------------------------------------------------------
// Byte vs. char offsets: positions must be byte offsets into the original
// `&str`, correct even when multi-byte UTF-8 characters precede a match.
// ---------------------------------------------------------------------------

#[test]
fn positions_are_byte_offsets_not_char_offsets() -> R {
    // "café" is 4 chars but 5 bytes ('é' = U+00E9 = 2 UTF-8 bytes), so any
    // byte/char confusion here would slice mid-character and this test would
    // panic, not just report a wrong number.
    let re = Regex::xpath("[0-9]+", "")?;
    let input = "café123bar456";
    assert_eq!(input.len(), 14); // byte length, for sanity
    let matches: Vec<_> = re.find_iter(input).collect::<Result<_, _>>()?;
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].range(), 5..8);
    assert_eq!(matches[0].as_str(), "123");
    assert_eq!(matches[1].range(), 11..14);
    assert_eq!(matches[1].as_str(), "456");
    // Slicing the original string with the reported range must reproduce
    // exactly the matched text, proving the offsets are valid char
    // boundaries in the *original* string.
    assert_eq!(&input[matches[0].range()], "123");
    assert_eq!(&input[matches[1].range()], "456");
    Ok(())
}

#[test]
fn astral_character_counts_as_one_char_for_group_positions() -> R {
    // An astral-plane code point is one `char` but 4 UTF-8 bytes.
    let re = Regex::xpath("(.)(.)", "")?;
    let input = "\u{1f600}x";
    let m = re.find(input)?.expect("should match");
    assert_eq!(m.group(1), Some(0..4));
    assert_eq!(m.group(2), Some(4..5));
    assert_eq!(m.as_str(), input);
    Ok(())
}

// ---------------------------------------------------------------------------
// Capturing-group positions, including groups that do not participate.
// ---------------------------------------------------------------------------

#[test]
fn group_positions_and_non_participation() -> R {
    let re = Regex::xpath("(a)(b)?", "")?;
    let matches: Vec<_> = re.find_iter("ab a").collect::<Result<_, _>>()?;
    assert_eq!(matches.len(), 2);

    assert_eq!(matches[0].as_str(), "ab");
    assert_eq!(matches[0].group_str(1), Some("a"));
    assert_eq!(matches[0].group_str(2), Some("b"));
    assert_eq!(matches[0].group(1), Some(0..1));
    assert_eq!(matches[0].group(2), Some(1..2));

    assert_eq!(matches[1].as_str(), "a");
    assert_eq!(matches[1].group_str(1), Some("a"));
    assert_eq!(matches[1].group_str(2), None); // group 2 did not participate
    assert_eq!(matches[1].group(2), None);
    Ok(())
}

#[test]
fn group_zero_is_not_a_capturing_group() -> R {
    let re = Regex::xpath("(a)", "")?;
    let m = re.find("a")?.expect("should match");
    // `range`/`start`/`end` are the accessors for the whole match; `group`
    // only answers for capturing groups 1..=group_count.
    assert_eq!(m.group(0), None);
    assert_eq!(m.group_count(), 1);
    assert_eq!(m.group(1), Some(0..1));
    Ok(())
}

#[test]
fn untaken_alternation_branch_leaves_its_group_unset() -> R {
    let re = Regex::xpath("(ab)|(a)", "")?;
    let matches: Vec<_> = re.find_iter("abcadabra").collect::<Result<_, _>>()?;
    // "ab" then "a" (from "cad"), then "a" (from "abra")... exercise at
    // least the first two, where the two branches alternate.
    assert_eq!(matches[0].as_str(), "ab");
    assert_eq!(matches[0].group(1), Some(0..2));
    assert_eq!(matches[0].group(2), None);
    Ok(())
}

#[test]
fn groups_iterator_matches_group_count() -> R {
    let re = Regex::xpath("(a)(b)(c)?", "")?;
    let m = re.find("ab")?.expect("should match");
    assert_eq!(m.group_count(), 3);
    let collected: Vec<_> = m.groups().collect();
    assert_eq!(collected.len(), 3);
    assert_eq!(collected, vec![Some(0..1), Some(1..2), None]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Zero-length-match advancement (`fn:analyze-string` / `fn:tokenize` rule):
// a zero-length match always still moves the scan on by exactly one `char`.
// ---------------------------------------------------------------------------

#[test]
fn zero_length_matches_advance_by_one_char_and_keep_scanning() -> R {
    // `x*` can match zero characters anywhere, so every position that is not
    // itself inside a run of 'x' produces an empty match, and a run of 'x'
    // produces one non-empty match — never an infinite loop, never a skipped
    // position.
    let re = Regex::xpath("x*", "")?;
    let matches: Vec<_> = re.find_iter("axxbx").collect::<Result<_, _>>()?;
    let ranges: Vec<_> = matches.iter().map(Match::range).collect();
    assert_eq!(ranges, [0..0, 1..3, 3..3, 4..5, 5..5]);
    for m in &matches {
        assert_eq!(&"axxbx"[m.range()], m.as_str());
    }
    Ok(())
}

#[test]
fn zero_length_match_at_very_end_of_input_terminates() -> R {
    // A pattern that can only match zero-length must still terminate rather
    // than loop forever once the scan reaches the end of the input.
    let re = Regex::xpath("x?", "")?;
    let matches: Vec<_> = re.find_iter("ab").collect::<Result<_, _>>()?;
    let ranges: Vec<_> = matches.iter().map(Match::range).collect();
    assert_eq!(ranges, [0..0, 1..1, 2..2]);
    Ok(())
}

#[test]
fn find_iter_does_not_reject_empty_matching_patterns() -> R {
    // Unlike `replace`/`tokenize` (FORX0003), `find_iter` itself never
    // rejects a pattern purely because it can match the empty string.
    let re = Regex::xpath("a*", "")?;
    assert!(re.find_iter("bbb").next().is_some());
    Ok(())
}

// ---------------------------------------------------------------------------
// Back-reference patterns: `find_iter` surfaces `ResourceLimit` rather than
// silently stopping.
// ---------------------------------------------------------------------------

#[test]
fn budget_exceeded_surfaces_as_an_error_item() -> R {
    let re = Regex::xpath("(a+)+\\1c", "")?;
    let input: String = std::iter::repeat_n('a', 40).collect();
    let limits = Limits {
        work: 2000,
        ..Limits::default()
    };
    let mut it = re.find_iter_with_limits(&input, limits);
    let first = it.next().expect("iterator should yield the error");
    assert!(matches!(
        first,
        Err(RegexError::ResourceLimit {
            stage: "work",
            limit: 2000
        })
    ));
    // The iterator must not keep trying after reporting the error.
    assert!(it.next().is_none());
    Ok(())
}

#[test]
fn find_iter_shares_one_budget_across_successful_matches() -> R {
    // Each individual search is cheap. A single bounded iterator must still
    // stop once the cumulative operation allowance is exhausted.
    let re = Regex::xpath("(a)\\1", "")?;
    let input = "aa ".repeat(200);
    let limits = Limits {
        work: 4000,
        ..Limits::default()
    };
    assert!(
        re.find_iter_with_limits("aa", limits)
            .next()
            .transpose()?
            .is_some()
    );
    let mut iter = re.find_iter_with_limits(&input, limits);
    let mut matched = 0;
    loop {
        match iter.next() {
            Some(Ok(found)) => {
                assert_eq!(found.as_str(), "aa");
                matched += 1;
            }
            Some(Err(RegexError::ResourceLimit {
                stage: "work",
                limit: 4000,
            })) => break,
            other => panic!("expected one shared-budget error, got {other:?}"),
        }
    }
    assert!(matched > 0 && matched < 200);
    assert!(iter.next().is_none());
    Ok(())
}

#[test]
fn back_reference_group_positions() -> R {
    let re = Regex::xpath("(a|b)\\1", "")?;
    let m = re.find("xxbbyy")?.expect("should match");
    assert_eq!(m.range(), 2..4);
    assert_eq!(m.group(1), Some(2..3));
    Ok(())
}

// ---------------------------------------------------------------------------
// XSD dialect: `find_iter` is reachable and behaves the same way; only
// `is_match`/`try_is_match` apply XSD's whole-string anchoring.
// ---------------------------------------------------------------------------

#[test]
fn find_iter_works_on_an_xsd_regex_too() -> R {
    let re = Regex::xsd("[0-9]+")?;
    let matches: Vec<_> = re.find_iter("a1b22").collect::<Result<_, _>>()?;
    let texts: Vec<&str> = matches.iter().map(|m| m.as_str()).collect();
    assert_eq!(texts, ["1", "22"]);
    Ok(())
}
