//! `oxixml-regex` — the regular-expression engine shared across the
//! [OxiXML](https://github.com/cool-japan/oxixml) workspace.
//!
//! Part of the COOLJAPAN Pure-Rust foundation for all XML and RDF processing.
//! This crate is `std`-only and depends only on
//! [`oxixml-unicode`](https://docs.rs/oxixml-unicode) for Unicode property data;
//! it contains **no external regex dependency** — the whole engine is
//! implemented here from the two W3C specifications.
//!
//! # Two dialects, one engine
//!
//! A [`Regex`] can be built in either of two flavours that a conforming XML
//! stack must implement:
//!
//! * [`Regex::xsd`] — the [XML Schema `pattern` facet][xsd] flavour. The pattern
//!   is *implicitly anchored to the whole string*, has no flags, no anchors, and
//!   no back-references, but supports character-class **range subtraction**
//!   (`[a-z-[aeiou]]`, nestable), the XML-name-aware escapes `\i` `\c`, and
//!   `\p{...}` category and `\p{Is...}` block escapes. It reads the pattern
//!   against XSD 1.1; [`Regex::xsd_versioned`] selects the stricter XSD 1.0
//!   grammar instead (see [`XsdVersion`]).
//! * [`Regex::xpath`] — the [XPath / XQuery `fn:matches`][fo] flavour. It adds
//!   the anchors `^` and `$`, the flags `s` `m` `i` `x` `q`, reluctant
//!   quantifiers (`a*?`), non-capturing groups (`(?:…)`), and back-references
//!   (`\1`…`\9`), and it is **not** implicitly anchored. F&O defines its
//!   character-class grammar by reference to XML Schema's, so this dialect is
//!   read against an [`XsdVersion`] too — [`DEFAULT_XSD_VERSION`] unless
//!   [`Regex::xpath_versioned`] says otherwise.
//!
//! Patterns without back-references compile to a linear-time Pike VM, so they
//! cannot suffer catastrophic back-tracking. Patterns *with* back-references run
//! on a back-tracking interpreter bounded by a step budget, which returns
//! [`RegexError::ResourceLimit`] rather than hanging. Compilation, input,
//! capture storage, and replacement output use the same typed limit errors.
//!
//! # Per-match, per-group positions
//!
//! [`Regex::is_match`], [`Regex::replace`], and [`Regex::tokenize`] answer
//! *whether* and *how* an input matches, but not *where*. [`Regex::find_iter`]
//! (and the single-match convenience [`Regex::find`]) fill that gap: they
//! yield a [`Match`] per non-overlapping match, each carrying the byte
//! position of the whole match and of every capturing group — the primitive
//! an implementation of [`fn:analyze-string`][as] needs.
//!
//! [as]: https://www.w3.org/TR/xpath-functions-31/#func-analyze-string
//!
//! # Errors
//!
//! Failures map onto the four W3C error codes through [`RegexError::forx_code`]:
//! `FORX0001` (flags), `FORX0002` (pattern), `FORX0003` (a `replace` or
//! `tokenize` pattern that matches the empty string), and `FORX0004` (an invalid
//! replacement string).
//!
//! [xsd]: https://www.w3.org/TR/xmlschema-2/#regexs
//! [fo]: https://www.w3.org/TR/xpath-functions-31/#regex-syntax
//!
//! # Example
//!
//! ```
//! use oxixml_regex::Regex;
//!
//! // XSD: implicitly anchored to the whole string.
//! let postcode = Regex::xsd("[0-9]{5}(-[0-9]{4})?")?;
//! assert!(postcode.is_match("12345"));
//! assert!(postcode.is_match("12345-6789"));
//! assert!(!postcode.is_match("12345-678"));   // must match the whole string
//!
//! // XSD range subtraction: consonants only.
//! let consonant = Regex::xsd("[a-z-[aeiou]]+")?;
//! assert!(consonant.is_match("rhythm"));
//! assert!(!consonant.is_match("aeiou"));
//!
//! // XPath: unanchored search with the case-insensitive flag.
//! let word = Regex::xpath("hello", "i")?;
//! assert!(word.is_match("Well, HELLO there"));
//!
//! // fn:replace with capture references.
//! let swap = Regex::xpath("(\\w+)\\s+(\\w+)", "")?;
//! assert_eq!(swap.replace("John Smith", "$2, $1")?, "Smith, John");
//!
//! // fn:tokenize.
//! let commas = Regex::xpath(",\\s*", "")?;
//! assert_eq!(commas.tokenize("1, 15, 24, 50")?, ["1", "15", "24", "50"]);
//! # Ok::<(), oxixml_regex::RegexError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod ast;
mod backtrack;
mod budget;
mod charclass;
mod error;
mod matches;
mod parser;
mod pikevm;
mod program;
mod replace;

use budget::Budget;
pub use budget::Limits;
pub use error::{ForxCode, RegexError};
pub use matches::{Match, Matches};

use backtrack::BtProgram;
use parser::{Dialect, Flags};
use pikevm::Captures;
use program::Program;

/// Which version of [XML Schema][xsd] a pattern is read against.
///
/// This governs *both* dialects. A `pattern` facet is read against the version
/// of the schema that declares it; an XPath pattern is read against the version
/// the processor implements, because [F&O 3.1 §5.6.1][fo] defines the XPath
/// syntax by reference to the XML Schema grammar rather than restating it.
///
/// The two Appendices give the same grammar except in a character class, where
/// XSD 1.1 relaxed two XSD 1.0 rules:
///
/// * a raw `-` may stand as a member anywhere in a character group, not only as
///   its first or last member — so `[a-a-x-x]`, `[a-c-1-4x-z-7-9]`, and
///   `[^a-d-b-c]` compile under 1.1 and are rejected under 1.0. This one
///   applies in both dialects.
/// * an unrecognised `\p{Is…}` block name matches every character rather than
///   being an error — so `\p{IsaA0-a9}` compiles under 1.1 and is rejected
///   under 1.0. This one is the **`pattern`-facet dialect's only**: the
///   XPath dialect rejects an unknown block name whatever the version, which
///   is what QT3 `fn-matches.re` `re00216` requires of that very pattern with
///   no `<dependency>` on either side.
///
/// Everything else, including range subtraction and the rule that an unescaped
/// `-` is never a range *endpoint*, is common to both.
///
/// [xsd]: https://www.w3.org/TR/xmlschema-2/#regexs
/// [fo]: https://www.w3.org/TR/xpath-functions-31/#regex-syntax
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XsdVersion {
    /// [XML Schema 1.0][spec], Appendix F.
    ///
    /// [spec]: https://www.w3.org/TR/xmlschema-2/#regexs
    V1_0,
    /// [XML Schema 1.1][spec], Appendix G.
    ///
    /// [spec]: https://www.w3.org/TR/xmlschema11-2/#regexs
    V1_1,
}

/// The XML Schema version [`Regex::xpath`] reads a pattern against.
///
/// An XPath processor answers for exactly one version — the suites write the
/// two readings as pairs over the same pattern and gate them on it. This
/// default preserves the upstream API; D3FEND explicitly selects XSD 1.1
/// with [`Regex::xpath_versioned`] or the bounded constructor.
///
/// This is deliberately *not* [`Regex::xsd`]'s default: a `pattern` facet is
/// read against the version of the schema document that carries it, which the
/// validator knows and passes to [`Regex::xsd_versioned`] explicitly.
pub const DEFAULT_XSD_VERSION: XsdVersion = XsdVersion::V1_0;

/// Which anchoring rule applies when deciding whether an input matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    /// The whole string must match (XSD dialect).
    Full,
    /// Any substring may match (XPath dialect).
    Search,
}

/// The compiled form of the pattern.
#[derive(Debug, Clone)]
enum Engine {
    /// A linear-time Pike VM (patterns without back-references).
    Pike(Program),
    /// A budgeted back-tracking interpreter (patterns with back-references).
    Backtrack(BtProgram),
}

/// A single match with its capture slots, in the engine's native
/// [`char`]-index coordinate space (an index into the `Vec<char>` the engine
/// matches over, not a byte offset). This is the shared internal
/// representation `replace`, `tokenize`, and [`matches::Matches`] all build
/// on; the public, byte-indexed [`Match`] is derived from it by
/// [`matches::Matches::next`].
pub(crate) struct RawMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) caps: Captures,
}

impl RawMatch {
    fn from_captures(caps: Captures) -> Option<RawMatch> {
        let start = caps.first().copied().flatten()?;
        let end = caps.get(1).copied().flatten()?;
        Some(RawMatch { start, end, caps })
    }
}

/// A compiled regular expression in either the XSD or the XPath dialect.
///
/// Build one with [`Regex::xsd`] or [`Regex::xpath`], then test inputs with
/// [`Regex::is_match`], or transform them with [`Regex::replace`] and
/// [`Regex::tokenize`].
#[derive(Debug, Clone)]
pub struct Regex {
    engine: Engine,
    mode: MatchMode,
    flags: Flags,
    group_count: usize,
    limits: Limits,
}

impl Regex {
    /// Compiles an XML Schema 1.1 pattern with default resource limits.
    pub fn xsd(pattern: &str) -> Result<Self, RegexError> {
        Self::xsd_versioned(pattern, XsdVersion::V1_1)
    }

    /// Compiles an XML Schema pattern using the selected grammar.
    pub fn xsd_versioned(pattern: &str, version: XsdVersion) -> Result<Self, RegexError> {
        Self::compile(pattern, Dialect::Xsd, version, "", Limits::default(), None)
    }

    /// Compiles an XPath pattern using the default XSD grammar.
    pub fn xpath(pattern: &str, flags: &str) -> Result<Self, RegexError> {
        Self::xpath_versioned(pattern, flags, DEFAULT_XSD_VERSION)
    }

    /// Compiles an XPath pattern using an explicit XSD grammar.
    pub fn xpath_versioned(
        pattern: &str,
        flags: &str,
        version: XsdVersion,
    ) -> Result<Self, RegexError> {
        Self::xpath_with_limits(pattern, flags, version, Limits::default())
    }

    /// Compiles a pattern with explicit resource limits. Limits are execution
    /// failures, never XPath expression errors. They also become the defaults
    /// for subsequent matching and replacement operations on this pattern.
    pub fn xpath_with_limits(
        pattern: &str,
        flags: &str,
        version: XsdVersion,
        limits: Limits,
    ) -> Result<Self, RegexError> {
        Self::compile(pattern, Dialect::Xpath, version, flags, limits, None)
    }

    /// Compiles a pattern with cooperative cancellation checked during work.
    pub fn xpath_with_cancel(
        pattern: &str,
        flags: &str,
        version: XsdVersion,
        limits: Limits,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, RegexError> {
        Self::compile(
            pattern,
            Dialect::Xpath,
            version,
            flags,
            limits,
            Some(cancelled),
        )
    }

    fn compile(
        pattern: &str,
        dialect: Dialect,
        version: XsdVersion,
        flags: &str,
        limits: Limits,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<Self, RegexError> {
        let mut budget = operation_budget(limits, cancelled);
        budget.check_pattern(pattern.len())?;
        budget.check_pattern(flags.len())?;
        budget.charge_work(flags.len())?;
        let flags = Flags::parse(flags)?;
        let parsed = parser::parse(pattern, dialect, version, flags, &mut budget)?;
        let engine = if parsed.ast.has_backref() {
            Engine::Backtrack(backtrack::compile(
                &parsed.ast,
                parsed.group_count,
                &mut budget,
            )?)
        } else {
            Engine::Pike(program::compile(
                &parsed.ast,
                parsed.group_count,
                &mut budget,
            )?)
        };
        Ok(Self {
            engine,
            mode: if dialect == Dialect::Xsd {
                MatchMode::Full
            } else {
                MatchMode::Search
            },
            flags,
            group_count: parsed.group_count,
            limits,
        })
    }

    /// Number of capturing groups, excluding group zero (the whole match).
    pub fn group_count(&self) -> usize {
        self.group_count
    }

    /// Compatibility convenience. Reasoning callers must use `try_is_match`
    /// because this method cannot expose execution failures.
    pub fn is_match(&self, input: &str) -> bool {
        self.try_is_match(input).unwrap_or(false)
    }

    /// Evaluates with the pattern's limits and preserves execution failures.
    pub fn try_is_match(&self, input: &str) -> Result<bool, RegexError> {
        self.try_is_match_with_limits(input, self.limits)
    }

    /// Evaluates with explicit operation limits.
    pub fn try_is_match_with_limits(
        &self,
        input: &str,
        limits: Limits,
    ) -> Result<bool, RegexError> {
        self.is_match_budgeted(input, &mut Budget::new(limits))
    }

    /// Evaluates with a cooperative cancellation callback.
    pub fn try_is_match_with_cancel(
        &self,
        input: &str,
        limits: Limits,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, RegexError> {
        self.is_match_budgeted(input, &mut Budget::with_cancel(limits, cancelled))
    }

    fn is_match_budgeted(&self, input: &str, budget: &mut Budget<'_>) -> Result<bool, RegexError> {
        let chars = input_chars(input, budget)?;
        let ci = self.flags.case_insensitive;
        let ml = self.flags.multiline;
        match &self.engine {
            Engine::Pike(program) => {
                let full = self.mode == MatchMode::Full;
                Ok(pikevm::run(program, &chars, 0, full, full, ci, ml, budget)?.is_some())
            }
            Engine::Backtrack(program) => match self.mode {
                MatchMode::Search => backtrack::is_match_search(program, &chars, ci, ml, budget),
                MatchMode::Full => backtrack::is_match_full(program, &chars, ci, ml, budget),
            },
        }
    }

    /// Iterates non-overlapping matches. Initialization or execution failures
    /// are yielded once, and one budget is shared by the entire iterator.
    pub fn find_iter<'r, 't>(&'r self, input: &'t str) -> Matches<'r, 't> {
        Matches::new(self, input, self.limits)
    }

    /// Iterates with explicit operation limits.
    pub fn find_iter_with_limits<'r, 't>(
        &'r self,
        input: &'t str,
        limits: Limits,
    ) -> Matches<'r, 't> {
        Matches::new(self, input, limits)
    }

    /// Finds the leftmost match, preserving execution errors.
    pub fn find<'t>(&self, input: &'t str) -> Result<Option<Match<'t>>, RegexError> {
        self.find_iter(input).next().transpose()
    }

    /// Applies the XPath replacement language with one operation budget.
    pub fn replace(&self, input: &str, replacement: &str) -> Result<String, RegexError> {
        self.replace_with_limits(input, replacement, self.limits)
    }

    /// Replaces with explicit execution limits.
    pub fn replace_with_limits(
        &self,
        input: &str,
        replacement: &str,
        limits: Limits,
    ) -> Result<String, RegexError> {
        self.replace_budgeted(input, replacement, &mut Budget::new(limits))
    }

    /// Replaces with cooperative cancellation.
    pub fn replace_with_cancel(
        &self,
        input: &str,
        replacement: &str,
        limits: Limits,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String, RegexError> {
        self.replace_budgeted(
            input,
            replacement,
            &mut Budget::with_cancel(limits, cancelled),
        )
    }

    fn replace_budgeted(
        &self,
        input: &str,
        replacement: &str,
        budget: &mut Budget<'_>,
    ) -> Result<String, RegexError> {
        // Validate independently of matches, including input with no match.
        let template =
            replace::Template::parse(replacement, self.group_count, self.flags.literal, budget)?;
        if self.matches_empty(budget)? {
            return Err(RegexError::MatchesEmptyString);
        }
        let chars = input_chars(input, budget)?;
        let mut out = String::new();
        let mut last = 0;
        while let Some(m) = self.find_from(&chars, last, budget)? {
            replace::append_chars(&mut out, &chars[last..m.start], budget)?;
            template.expand_into(&mut out, &chars, &m.caps, budget)?;
            // Empty matches were rejected above; retaining this check avoids
            // an accidental infinite loop if an engine invariant is broken.
            if m.end == m.start {
                return Err(RegexError::MatchesEmptyString);
            }
            last = m.end;
            if last >= chars.len() {
                break;
            }
        }
        replace::append_chars(&mut out, &chars[last..], budget)?;
        Ok(out)
    }

    /// Splits the input at matches with one shared operation budget.
    pub fn tokenize(&self, input: &str) -> Result<Vec<String>, RegexError> {
        let mut budget = Budget::new(self.limits);
        let chars = input_chars(input, &mut budget)?;
        if chars.is_empty() {
            return Ok(Vec::new());
        }
        if self.matches_empty(&mut budget)? {
            return Err(RegexError::MatchesEmptyString);
        }
        let mut result = Vec::new();
        let mut last = 0;
        let mut total_output = 0usize;
        while let Some(m) = self.find_from(&chars, last, &mut budget)? {
            push_token(
                &mut result,
                &chars[last..m.start],
                &mut total_output,
                &mut budget,
            )?;
            if m.end == m.start {
                return Err(RegexError::MatchesEmptyString);
            }
            last = m.end;
        }
        push_token(&mut result, &chars[last..], &mut total_output, &mut budget)?;
        Ok(result)
    }

    pub(crate) fn find_from(
        &self,
        chars: &[char],
        pos: usize,
        budget: &mut Budget<'_>,
    ) -> Result<Option<RawMatch>, RegexError> {
        let ci = self.flags.case_insensitive;
        let ml = self.flags.multiline;
        let caps = match &self.engine {
            Engine::Pike(program) => {
                pikevm::run(program, chars, pos, false, false, ci, ml, budget)?
            }
            Engine::Backtrack(program) => {
                backtrack::find_from(program, chars, pos, ci, ml, budget)?
            }
        };
        Ok(caps.and_then(RawMatch::from_captures))
    }

    fn matches_empty(&self, budget: &mut Budget<'_>) -> Result<bool, RegexError> {
        Ok(self.find_from(&[], 0, budget)?.is_some())
    }
}

fn operation_budget(limits: Limits, cancelled: Option<&dyn Fn() -> bool>) -> Budget<'_> {
    match cancelled {
        Some(check) => Budget::with_cancel(limits, check),
        None => Budget::new(limits),
    }
}

pub(crate) fn allocation_error(budget: &Budget<'_>) -> RegexError {
    RegexError::ResourceLimit {
        stage: "regular expression allocation",
        limit: budget.limits.allocation_bytes,
    }
}

pub(crate) fn input_chars(input: &str, budget: &mut Budget<'_>) -> Result<Vec<char>, RegexError> {
    budget.check_input(input.len())?;
    budget.charge_work(input.len())?;
    let count = input.chars().count();
    let bytes = count
        .checked_mul(std::mem::size_of::<char>())
        .ok_or_else(|| allocation_error(budget))?;
    budget.charge_alloc(bytes)?;
    let mut chars = Vec::new();
    chars
        .try_reserve_exact(count)
        .map_err(|_| allocation_error(budget))?;
    budget.charge_work(input.len())?;
    chars.extend(input.chars());
    Ok(chars)
}

fn push_token(
    result: &mut Vec<String>,
    chars: &[char],
    total: &mut usize,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let mut token = String::new();
    replace::append_chars(&mut token, chars, budget)?;
    *total = total
        .checked_add(token.len())
        .ok_or_else(|| allocation_error(budget))?;
    budget.check_output(*total)?;
    if result.len() == result.capacity() {
        let capacity = result.capacity().saturating_mul(2).max(4);
        budget.charge_alloc_product(capacity, std::mem::size_of::<String>())?;
        budget.charge_work(result.len())?;
        result
            .try_reserve_exact(capacity - result.len())
            .map_err(|_| allocation_error(budget))?;
    }
    result.push(token);
    Ok(())
}
