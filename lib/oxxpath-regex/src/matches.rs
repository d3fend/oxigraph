//! The public per-match, per-capturing-group API: [`Regex::find_iter`],
//! [`Matches`], and [`Match`].
//!
//! This is the primitive [`fn:analyze-string`][as] (F&O 3.1 §5.6.5) needs and
//! that [`Regex::is_match`], [`Regex::replace`], and [`Regex::tokenize`] do
//! not expose: the position of the whole match *and* of every capturing
//! group, for every non-overlapping match in the input, not just whether (or
//! how) the input matches.
//!
//! [as]: https://www.w3.org/TR/xpath-functions-31/#func-analyze-string

use std::iter::FusedIterator;
use std::ops::Range;

use crate::budget::Budget;
use crate::{Limits, RawMatch, Regex, RegexError, allocation_error, input_chars};

/// A single non-overlapping match, produced by [`Regex::find_iter`] or
/// [`Regex::find`].
///
/// All positions ([`Match::range`], [`Match::start`], [`Match::end`], and the
/// group accessors) are **byte offsets** into the original `&str` passed to
/// [`Regex::find_iter`], so `&input[m.range()]` (equivalently
/// [`Match::as_str`]) always slices it correctly. The engine matches by
/// Unicode scalar value (`char`) internally, and every reported offset falls
/// on a `char` boundary, so slicing can never panic on a UTF-8 boundary
/// violation.
///
/// # Zero-length matches
///
/// A pattern that can match the empty string (for example `a*` or `x?`) can
/// produce a zero-length [`Match`] (`m.start() == m.end()`). When
/// [`Regex::find_iter`] yields one, the scan for the *next* match still
/// resumes one `char` further on, never at the same position — otherwise the
/// same zero-length match would repeat forever. This is the rule
/// [`fn:analyze-string`][as] §5.6.5 and [`fn:tokenize`][tok] §5.6.3 both use.
/// Unlike [`Regex::replace`] and [`Regex::tokenize`], `find_iter` does *not*
/// reject a pattern that can match the empty string — `FORX0003` is specific
/// to those two functions.
///
/// [as]: https://www.w3.org/TR/xpath-functions-31/#func-analyze-string
/// [tok]: https://www.w3.org/TR/xpath-functions-31/#func-tokenize
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match<'t> {
    text: &'t str,
    range: Range<usize>,
    /// Byte range of capturing group `i` (1-based) at index `i - 1`; `None`
    /// when group `i` did not participate in this match.
    groups: Vec<Option<Range<usize>>>,
}

impl<'t> Match<'t> {
    /// Builds a [`Match`] from already-resolved byte positions.
    pub(crate) fn new(
        text: &'t str,
        range: Range<usize>,
        groups: Vec<Option<Range<usize>>>,
    ) -> Match<'t> {
        Match {
            text,
            range,
            groups,
        }
    }

    /// The byte range of the whole match within the original input.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// The byte offset of the start of the match.
    #[must_use]
    pub fn start(&self) -> usize {
        self.range.start
    }

    /// The byte offset of the end of the match (exclusive).
    #[must_use]
    pub fn end(&self) -> usize {
        self.range.end
    }

    /// The text of the whole match.
    ///
    /// # Examples
    ///
    /// ```
    /// use oxixml_regex::Regex;
    ///
    /// let re = Regex::xpath("[0-9]+", "")?;
    /// let m = re.find("abc123def")?.expect("should match");
    /// assert_eq!(m.as_str(), "123");
    /// # Ok::<(), oxixml_regex::RegexError>(())
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &'t str {
        &self.text[self.range.clone()]
    }

    /// Whether the match is zero-length (`start() == end()`). See the
    /// [`Match`] documentation for how iteration advances past one of these.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.range.start == self.range.end
    }

    /// The number of capturing groups in the pattern that produced this
    /// match. Group indices `1..=group_count()` are valid arguments to
    /// [`Match::group`]; this excludes group `0`, the whole match, which is
    /// always present and is what [`Match::range`] already reports.
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// The byte range of capturing group `index`, or `None` if `index` is
    /// `0` or out of range, or if the group did not participate in this
    /// match (for example, a branch of an alternation that was not taken, or
    /// an optional group that matched zero times).
    ///
    /// Note that `index` counts capturing groups only — a `(?:...)`
    /// non-capturing group is never assigned a number and can never be
    /// queried this way.
    ///
    /// # Examples
    ///
    /// ```
    /// use oxixml_regex::Regex;
    ///
    /// let re = Regex::xpath("(a+)(b+)?", "")?;
    /// let m = re.find("aaa")?.expect("should match");
    /// assert_eq!(m.group(1), Some(0..3));
    /// assert_eq!(m.group(2), None); // group 2 did not participate
    /// assert_eq!(m.group(0), None); // 0 is not a capturing-group index
    /// # Ok::<(), oxixml_regex::RegexError>(())
    /// ```
    #[must_use]
    pub fn group(&self, index: usize) -> Option<Range<usize>> {
        if index == 0 {
            return None;
        }
        self.groups.get(index - 1)?.clone()
    }

    /// The text of capturing group `index`; see [`Match::group`].
    #[must_use]
    pub fn group_str(&self, index: usize) -> Option<&'t str> {
        self.group(index).map(|r| &self.text[r])
    }

    /// An iterator over the byte ranges of every capturing group, in order
    /// from `1` to [`Match::group_count`] (`None` for a group that did not
    /// participate). Group `0`, the whole match, is not included — use
    /// [`Match::range`] for that.
    pub fn groups(&self) -> impl ExactSizeIterator<Item = Option<Range<usize>>> + '_ {
        self.groups.iter().cloned()
    }
}

/// Successive non-overlapping matches sharing one operation allowance.
/// Initialization and runtime errors are yielded once; the iterator then ends.
pub struct Matches<'r, 't> {
    regex: &'r Regex,
    text: &'t str,
    chars: Vec<char>,
    byte_offsets: Vec<usize>,
    pos: usize,
    done: bool,
    error: Option<RegexError>,
    budget: Budget<'r>,
}

impl<'r, 't> Matches<'r, 't> {
    pub(crate) fn new(regex: &'r Regex, text: &'t str, limits: Limits) -> Self {
        let mut budget = Budget::new(limits);
        let initialized = (|| {
            let chars = input_chars(text, &mut budget)?;
            let count = chars
                .len()
                .checked_add(1)
                .ok_or_else(|| allocation_error(&budget))?;
            budget.charge_alloc(
                count
                    .checked_mul(std::mem::size_of::<usize>())
                    .ok_or_else(|| allocation_error(&budget))?,
            )?;
            budget.charge_work(text.len())?;
            let mut offsets = Vec::new();
            offsets
                .try_reserve_exact(count)
                .map_err(|_| allocation_error(&budget))?;
            offsets.extend(text.char_indices().map(|(byte, _)| byte));
            offsets.push(text.len());
            Ok::<_, RegexError>((chars, offsets))
        })();
        let (chars, byte_offsets, error) = match initialized {
            Ok((chars, offsets)) => (chars, offsets, None),
            Err(error) => (Vec::new(), Vec::new(), Some(error)),
        };
        Self {
            regex,
            text,
            chars,
            byte_offsets,
            pos: 0,
            done: false,
            error,
            budget,
        }
    }

    fn convert_and_advance(&mut self, raw: RawMatch) -> Result<Match<'t>, RegexError> {
        let count = self.regex.group_count();
        self.budget.charge_work(count)?;
        self.budget.charge_alloc(
            count
                .checked_mul(std::mem::size_of::<Option<Range<usize>>>())
                .ok_or_else(|| allocation_error(&self.budget))?,
        )?;
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(count)
            .map_err(|_| allocation_error(&self.budget))?;
        for i in 1..=count {
            let start = raw.caps.get(2 * i).copied().flatten();
            let end = raw.caps.get(2 * i + 1).copied().flatten();
            groups.push(match (start, end) {
                (Some(s), Some(e)) => Some(self.byte_offsets[s]..self.byte_offsets[e]),
                _ => None,
            });
        }
        let range = self.byte_offsets[raw.start]..self.byte_offsets[raw.end];
        if raw.end == raw.start {
            if raw.end >= self.chars.len() {
                self.done = true;
            } else {
                self.pos = raw.end + 1;
            }
        } else {
            self.pos = raw.end;
        }
        Ok(Match::new(self.text, range, groups))
    }
}

impl<'r, 't> Iterator for Matches<'r, 't> {
    type Item = Result<Match<'t>, RegexError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if let Some(error) = self.error.take() {
            self.done = true;
            return Some(Err(error));
        }
        let next = self
            .regex
            .find_from(&self.chars, self.pos, &mut self.budget)
            .and_then(|raw| raw.map(|raw| self.convert_and_advance(raw)).transpose());
        match next {
            Ok(Some(found)) => Some(Ok(found)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}
impl FusedIterator for Matches<'_, '_> {}
