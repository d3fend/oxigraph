//! The recursive-descent parser shared by both dialects.
//!
//! The grammar is the one given in [XML Schema Part 2, Appendix G][xsd] for the
//! XSD dialect and in [*XPath and XQuery Functions and Operators 3.1*][fo] §5.6
//! for the XPath dialect. The two differ only in a handful of places, all
//! selected by [`Dialect`]:
//!
//! * `^` and `$` are literal characters in XSD but anchors in XPath.
//! * Reluctant quantifiers (`*?`), non-capturing groups (`(?:...)`), and
//!   back-references (`\1`) exist only in XPath.
//! * XPath honours the `s`, `m`, `i`, `x`, and `q` [`Flags`].
//!
//! The XSD dialect is itself split by [`XsdVersion`]: XSD 1.1's Appendix G
//! accepts two character-class shapes that XSD 1.0's Appendix F rejects (a bare
//! `-` chained between ranges, and an unrecognised `\p{Is…}` block name). Every
//! other rule is shared by both versions.
//!
//! [xsd]: https://www.w3.org/TR/xmlschema-2/#regexs
//! [fo]: https://www.w3.org/TR/xpath-functions-31/#regex-syntax

use crate::XsdVersion;
use crate::ast::Ast;
use crate::budget::Budget;
use crate::charclass::{
    ClassExpr, ClassItem, PropName, Shorthand, category_from_abbreviation, resolve_block_bounded,
};
use crate::error::RegexError;

/// Which regular-expression dialect a pattern is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// The XML Schema `pattern` facet flavour: whole-string, no flags, no
    /// anchors, no back-references.
    Xsd,
    /// The XPath / XQuery `fn:matches` flavour.
    Xpath,
}

/// The parsed `flags` string of an XPath regular expression.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    /// `s` — dot-all mode: `.` also matches line feed and carriage return.
    pub dot_all: bool,
    /// `m` — multi-line mode: `^` and `$` match at internal line breaks.
    pub multiline: bool,
    /// `i` — case-insensitive matching.
    pub case_insensitive: bool,
    /// `x` — ignore unescaped whitespace outside character classes.
    pub ignore_whitespace: bool,
    /// `q` — treat the whole pattern as a literal string.
    pub literal: bool,
}

impl Flags {
    /// Parses an XPath `flags` string, rejecting any character other than
    /// `s`, `m`, `i`, `x`, or `q` with [`RegexError::InvalidFlags`].
    pub(crate) fn parse(flags: &str) -> Result<Flags, RegexError> {
        let mut out = Flags::default();
        for c in flags.chars() {
            match c {
                's' => out.dot_all = true,
                'm' => out.multiline = true,
                'i' => out.case_insensitive = true,
                'x' => out.ignore_whitespace = true,
                'q' => out.literal = true,
                other => {
                    return Err(RegexError::InvalidFlags(format!(
                        "unsupported flag {other:?}"
                    )));
                }
            }
        }
        Ok(out)
    }
}

/// The outcome of a successful parse.
pub(crate) struct Parsed {
    /// The syntax tree.
    pub ast: Ast,
    /// The number of capturing groups in the pattern.
    pub group_count: usize,
}

/// Parses `pattern` in the given `dialect` and XSD `version` with the given
/// `flags`.
///
/// `version` governs the character-class rules in *both* dialects: F&O 3.1
/// §5.6.1 defines the XPath syntax by reference to the XML Schema grammar, so
/// which of the two Appendices applies is the XSD version the processor
/// implements rather than a property of the call. See [`XsdVersion`].
pub(crate) fn parse(
    pattern: &str,
    dialect: Dialect,
    version: XsdVersion,
    flags: Flags,
    budget: &mut Budget<'_>,
) -> Result<Parsed, RegexError> {
    budget.check_pattern(pattern.len())?;
    // Source characters, parser nodes, class members, vector capacity growth,
    // and temporary property strings are all linear in the source byte count.
    // Admit a conservative envelope before any source/AST allocation.
    budget.charge_work_product(pattern.len(), 4)?;
    budget.charge_alloc(4 * std::mem::size_of::<Ast>())?;
    budget.charge_alloc_product(
        pattern.len(),
        4 * std::mem::size_of::<Ast>() + 2 * std::mem::size_of::<ClassItem>() + 64,
    )?;
    // The `q` flag short-circuits all metacharacter handling.
    if flags.literal {
        let ast = Ast::Concat(pattern.chars().map(Ast::Literal).collect());
        return Ok(Parsed {
            ast,
            group_count: 0,
        });
    }

    let source: Vec<char> = if flags.ignore_whitespace {
        strip_whitespace(pattern)
    } else {
        pattern.chars().collect()
    };

    let total_groups = count_capturing_groups(&source, dialect);
    budget.check_captures(total_groups)?;

    let mut parser = Parser {
        chars: source,
        pos: 0,
        dialect,
        version,
        flags,
        next_group: 1,
        budget,
        closed: vec![false; total_groups],
        depth: 0,
    };
    let ast = parser.parse_alternation()?;
    if parser.pos != parser.chars.len() {
        // The only way to stop early is an unbalanced ')'.
        return Err(RegexError::InvalidPattern(
            "unbalanced ')' in pattern".to_owned(),
        ));
    }
    Ok(Parsed {
        ast,
        group_count: total_groups,
    })
}

/// Removes whitespace that lies outside character classes, applying the `x`
/// flag.
///
/// F&O 3.1 §5.6.1 states the rule without an exception: "whitespace characters
/// ... in the regular expression are removed prior to matching with one
/// exception: whitespace characters within character class expressions are not
/// removed". A backslash therefore does *not* protect a following space — it
/// binds to whatever non-whitespace character comes next, which is what makes
/// `matches("hello world", "hello\ sworld", "x")` true (QT3
/// `K2-MatchesFunc-1`): the pattern is `hello\sworld`.
fn strip_whitespace(pattern: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    let mut class_depth = 0usize;
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push('\\');
            if class_depth == 0 {
                while chars.peek().copied().is_some_and(is_pattern_whitespace) {
                    chars.next();
                }
            }
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if class_depth == 0 && is_pattern_whitespace(c) {
            continue;
        }
        if c == '[' {
            class_depth += 1;
        } else if c == ']' {
            class_depth = class_depth.saturating_sub(1);
        }
        out.push(c);
    }
    out
}

fn is_pattern_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Counts capturing groups in a single scan so back-references can be resolved
/// against the total. Non-capturing `(?:` groups and escaped `\(` do not count;
/// parentheses inside a character class do not count.
fn count_capturing_groups(chars: &[char], dialect: Dialect) -> usize {
    let mut count = 0;
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                continue;
            }
            '[' => class_depth += 1,
            ']' => class_depth = class_depth.saturating_sub(1),
            '(' if class_depth == 0 => {
                let non_capturing = dialect == Dialect::Xpath
                    && chars.get(i + 1) == Some(&'?')
                    && chars.get(i + 2) == Some(&':');
                if !non_capturing {
                    count += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    count
}

/// The parser state.
struct Parser<'b, 'a> {
    chars: Vec<char>,
    pos: usize,
    dialect: Dialect,
    /// Which XSD grammar the pattern is read against — see
    /// [`Parser::is_xsd_1_0`] for the one rule it decides, and
    /// [`interpret_property_name`] for the one it does not.
    version: XsdVersion,
    flags: Flags,
    next_group: usize,
    budget: &'b mut Budget<'a>,
    /// Which capturing groups have had their closing `)` parsed already, by
    /// group number (`closed[N - 1]` is group `N`).
    ///
    /// A back-reference `\N` is only valid once group `N` itself is closed,
    /// which rules out both a forward reference (the group's `(` has not even
    /// been seen yet) and a self-reference (the backref sits inside its own
    /// group, before that group's own `)`). A *count* of closed groups is not
    /// enough, because groups nest: in `((#)abc\1)$` the inner group `2`
    /// closes first, so one group is closed by the time `\1` is read even
    /// though group `1` is still open. See [`Parser::parse_backref`].
    closed: Vec<bool>,
    /// The number of nested constructs (`(...)` groups and `[...-[...]]`
    /// character-class subtractions) whose parse is currently in progress.
    ///
    /// Both are parsed by recursive descent, so without a ceiling a pattern
    /// such as `((((...a...))))` — which an untrusted `xs:pattern` facet or an
    /// `fn:matches` argument can trivially contain — would consume one Rust
    /// stack frame per nesting level and abort the whole process with a stack
    /// overflow instead of returning a [`RegexError`]. See the configured nesting limit.
    depth: u32,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn is_xpath(&self) -> bool {
        self.dialect == Dialect::Xpath
    }

    /// Whether the strict XSD 1.0 character-class rules apply.
    ///
    /// Both dialects consult the version. F&O 3.1 §5.6.1 defines the XPath
    /// syntax by reference to the XML Schema grammar, and both W3C suites
    /// write the two readings as *pairs over the same pattern*, keyed on the
    /// XSD version the processor implements rather than on the dialect: QT3
    /// `fn-matches.re` `re00056` carries `<dependency type="xsd-version"
    /// value="1.0"/>` and expects `FORX0002` where `re00056a` carries
    /// `value="1.1"` and expects a match, and the XSLT 3.0 catalogue's
    /// `regex-syntax-0056` carries `<feature value="XSD_1.1"
    /// satisfied="false"/>` against `-0056a`'s `satisfied="true"`. A
    /// processor answers for exactly one version, so `Regex::xpath` names the
    /// one it implements and `Regex::xpath_versioned` names another.
    fn is_xsd_1_0(&self) -> bool {
        self.version == XsdVersion::V1_0
    }

    /// `alternation ::= sequence ('|' sequence)*`
    fn parse_alternation(&mut self) -> Result<Ast, RegexError> {
        let mut branches = vec![self.parse_sequence()?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.parse_sequence()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap_or(Ast::Empty))
        } else {
            Ok(Ast::Alt(branches))
        }
    }

    /// `sequence ::= piece*`
    fn parse_sequence(&mut self) -> Result<Ast, RegexError> {
        let mut pieces = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            pieces.push(self.parse_piece()?);
        }
        if pieces.is_empty() {
            Ok(Ast::Empty)
        } else if pieces.len() == 1 {
            Ok(pieces.pop().unwrap_or(Ast::Empty))
        } else {
            Ok(Ast::Concat(pieces))
        }
    }

    /// `piece ::= atom quantifier?`
    fn parse_piece(&mut self) -> Result<Ast, RegexError> {
        self.budget.charge_work(1)?;
        let atom = self.parse_atom()?;
        let (min, max) = match self.peek() {
            Some('?') => {
                self.bump();
                (0, Some(1))
            }
            Some('*') => {
                self.bump();
                (0, None)
            }
            Some('+') => {
                self.bump();
                (1, None)
            }
            Some('{') => {
                // Only a quantifier when it is a well-formed quantity; a lone
                // '{' in atom position has already been consumed by parse_atom.
                self.bump();
                self.parse_quantity()?
            }
            _ => return Ok(atom),
        };
        let greedy = if self.is_xpath() && self.peek() == Some('?') {
            self.bump();
            false
        } else {
            true
        };
        if max.is_some_and(|m| m < min) {
            return Err(RegexError::InvalidPattern(
                "quantifier upper bound is less than lower bound".to_owned(),
            ));
        }
        Ok(Ast::Repeat {
            inner: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    /// Parses the interior of a `{...}` quantifier (the opening brace is already
    /// consumed). Returns `(min, max)`.
    fn parse_quantity(&mut self) -> Result<(u32, Option<u32>), RegexError> {
        let min = self.parse_number()?;
        match self.peek() {
            Some('}') => {
                self.bump();
                Ok((min, Some(min)))
            }
            Some(',') => {
                self.bump();
                if self.peek() == Some('}') {
                    self.bump();
                    Ok((min, None))
                } else {
                    let max = self.parse_number()?;
                    if self.peek() != Some('}') {
                        return Err(RegexError::InvalidPattern(
                            "expected '}' to close quantifier".to_owned(),
                        ));
                    }
                    self.bump();
                    Ok((min, Some(max)))
                }
            }
            _ => Err(RegexError::InvalidPattern(
                "malformed quantifier".to_owned(),
            )),
        }
    }

    /// Parses decimal digits; unsupported numeric width is a resource limit.
    fn parse_number(&mut self) -> Result<u32, RegexError> {
        let start = self.pos;
        let mut value: u32 = 0;
        while let Some(c) = self.peek() {
            if let Some(d) = c.to_digit(10) {
                self.bump();
                value = value.checked_mul(10).and_then(|v| v.checked_add(d)).ok_or(
                    RegexError::ResourceLimit {
                        stage: "quantifier width",
                        limit: u32::MAX as usize,
                    },
                )?;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(RegexError::InvalidPattern(
                "expected a decimal number in quantifier".to_owned(),
            ));
        }
        Ok(value)
    }

    /// `atom ::= Char | charClass | group | anchor | escape`
    fn parse_atom(&mut self) -> Result<Ast, RegexError> {
        let c = match self.peek() {
            Some(c) => c,
            None => {
                return Err(RegexError::InvalidPattern(
                    "unexpected end of pattern".to_owned(),
                ));
            }
        };
        match c {
            '(' => self.parse_group(),
            '[' => Ok(Ast::Class(self.parse_class()?)),
            '.' => {
                self.bump();
                if self.flags.dot_all {
                    Ok(Ast::Class(ClassExpr::any()))
                } else {
                    Ok(Ast::Class(ClassExpr::dot()))
                }
            }
            '\\' => self.parse_escape_atom(),
            '^' => {
                self.bump();
                if self.is_xpath() {
                    Ok(Ast::AnchorStart)
                } else {
                    Ok(Ast::Literal('^'))
                }
            }
            '$' => {
                self.bump();
                if self.is_xpath() {
                    Ok(Ast::AnchorEnd)
                } else {
                    Ok(Ast::Literal('$'))
                }
            }
            '?' | '*' | '+' => Err(RegexError::InvalidPattern(format!(
                "quantifier {c:?} has no preceding atom"
            ))),
            ')' => Err(RegexError::InvalidPattern(
                "unbalanced ')' in pattern".to_owned(),
            )),
            // `Char` (XSD Appendix G.3 / XPath F&O 3.1 §5.6.1) excludes both
            // '{' and '}': a literal brace must be written `\{` / `\}`. A
            // bare '{' reaching atom position means either nothing precedes
            // it at all, or the previous piece already consumed its own
            // quantifier (so this would be a second, invalid quantifier on
            // the same atom, e.g. `a*{0,5}`); a valid `{n,m}` quantifier
            // suffix is recognised earlier, in `parse_piece`, and never
            // reaches this arm. Both the XPath and the XSD test suites
            // confirm this is invalid in *both* dialects, contradicting the
            // common assumption that XSD is lenient here: QT3 `fn-matches.re`
            // `re00019` (`{1}a`), `re00567`-`re00569` (`{5`, `{5,`, `{5,6`),
            // `re00686`-`re00688` (`a*{0,5}` and friends), and the XML Schema
            // Test Suite's `msData/regex/reB65` (`{1}a`), `RegexTest_24`-`26`
            // (`{5`, `{5,`, `{5,6`), and `RegexTest_311`-`313` (`a*{0,5}` and
            // friends) all expect `FORX0002`.
            '{' | '}' => Err(RegexError::InvalidPattern(format!(
                "'{c}' must be escaped (\\{c}) or used only as part of a {{n,m}} quantifier"
            ))),
            // Likewise ']' is excluded from `Char`; unescaped, it may only
            // close a character class. QT3 `re00804` (`a]`) and the XML
            // Schema Test Suite's `RegexTest_586`/`755`/`1070`/`1239` (all
            // `a]`) expect `FORX0002`.
            ']' => Err(RegexError::InvalidPattern(
                "unescaped ']' has no matching '['; write \\] for a literal ']'".to_owned(),
            )),
            other => {
                self.bump();
                Ok(Ast::Literal(other))
            }
        }
    }

    /// Enters one level of recursive nesting, rejecting the pattern once
    /// the configured nesting bound is exceeded. Every successful call must be paired
    /// with [`Parser::leave_nesting`]; on failure the error propagates out of
    /// the whole parse, so the counter is never observed again.
    fn enter_nesting(&mut self) -> Result<(), RegexError> {
        self.depth += 1;
        self.budget.check_nesting(self.depth as usize)
    }

    /// Leaves one level of recursive nesting entered by [`Parser::enter_nesting`].
    fn leave_nesting(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Parses `'(' '?:'? alternation ')'`, bounding the recursion depth.
    fn parse_group(&mut self) -> Result<Ast, RegexError> {
        self.enter_nesting()?;
        let result = self.parse_group_inner();
        self.leave_nesting();
        result
    }

    /// The body of [`Parser::parse_group`], called with the nesting level
    /// already accounted for.
    fn parse_group_inner(&mut self) -> Result<Ast, RegexError> {
        self.bump(); // '('
        let mut index = None;
        if self.is_xpath() && self.peek() == Some('?') && self.peek_at(1) == Some(':') {
            self.bump();
            self.bump();
        } else {
            index = Some(self.next_group);
            self.next_group += 1;
        }
        let inner = self.parse_alternation()?;
        if self.peek() != Some(')') {
            return Err(RegexError::InvalidPattern(
                "missing ')' to close group".to_owned(),
            ));
        }
        self.bump(); // ')'
        if let Some(index) = index {
            // Recursive descent already parsed `inner` (and thus any nested
            // groups) before reaching here, so a group is marked closed
            // exactly when its own `)` has been consumed: what
            // `parse_backref` needs to decide whether the group it names is
            // "already closed" at a given point in the pattern.
            if let Some(slot) = self.closed.get_mut(index - 1) {
                *slot = true;
            }
        }
        Ok(Ast::Group {
            index,
            inner: Box::new(inner),
        })
    }

    /// Parses an escape in atom position: single-character escapes become
    /// literals, class escapes become classes, and `\N` becomes a
    /// back-reference (XPath only).
    fn parse_escape_atom(&mut self) -> Result<Ast, RegexError> {
        self.bump(); // '\\'
        let n = match self.peek() {
            Some(c) => c,
            None => {
                return Err(RegexError::InvalidPattern(
                    "pattern ends with a backslash".to_owned(),
                ));
            }
        };
        if let Some(ch) = self.single_char_escape(n) {
            self.bump();
            return Ok(Ast::Literal(ch));
        }
        if let Some(sh) = shorthand_escape(n) {
            self.bump();
            return Ok(Ast::Class(ClassExpr::single_item(ClassItem::Shorthand(sh))));
        }
        if n == 'p' || n == 'P' {
            let (name, negated) = self.parse_property()?;
            return Ok(Ast::Class(ClassExpr::single_item(ClassItem::Prop {
                name,
                negated,
            })));
        }
        if n.is_ascii_digit() && n != '0' {
            if self.is_xpath() {
                return self.parse_backref();
            }
            return Err(RegexError::InvalidPattern(
                "back-references are not allowed in the XSD dialect".to_owned(),
            ));
        }
        Err(RegexError::InvalidPattern(format!(
            "unrecognised escape \\{n}"
        )))
    }

    /// The longest decimal prefix naming an already opened capturing group
    /// forms the back-reference; any remaining digits are ordinary literals.
    /// The selected group must also have closed before this position.
    fn parse_backref(&mut self) -> Result<Ast, RegexError> {
        let first = self.bump().and_then(|c| c.to_digit(10)).unwrap_or(0) as usize;
        if first >= self.next_group {
            return Err(RegexError::InvalidPattern(format!(
                "back-reference \\{first} has no matching group"
            )));
        }
        let mut number = first;
        while let Some(c) = self.peek() {
            if let Some(d) = c.to_digit(10) {
                let candidate = number.saturating_mul(10).saturating_add(d as usize);
                if candidate < self.next_group {
                    number = candidate;
                    self.bump();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if number == 0 || !self.closed.get(number - 1).copied().unwrap_or(false) {
            return Err(RegexError::InvalidPattern(format!(
                "back-reference \\{number} refers to a group that has not yet been closed"
            )));
        }
        Ok(Ast::Backref(number))
    }

    /// Recognises a single-character escape, returning the character it denotes.
    fn single_char_escape(&self, n: char) -> Option<char> {
        match n {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\\' | '|' | '.' | '-' | '^' | '?' | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' => {
                Some(n)
            }
            '$' if self.is_xpath() => Some('$'),
            _ => None,
        }
    }

    /// Parses `\p{...}` / `\P{...}`, returning the property and whether it was
    /// the negating `\P` form.
    fn parse_property(&mut self) -> Result<(PropName, bool), RegexError> {
        let letter = self.bump().unwrap_or('p');
        let negated = letter == 'P';
        if self.peek() != Some('{') {
            return Err(RegexError::InvalidPattern(
                "expected '{' after \\p".to_owned(),
            ));
        }
        self.bump();
        let mut name = String::new();
        loop {
            match self.bump() {
                Some('}') => break,
                Some(c) => name.push(c),
                None => {
                    return Err(RegexError::InvalidPattern(
                        "unterminated \\p{...} escape".to_owned(),
                    ));
                }
            }
        }
        let prop = interpret_property_name(&name, self.dialect, self.version, self.budget)?;
        Ok((prop, negated))
    }

    /// `charClass ::= '[' charGroup ']'`, including negation, ranges, class
    /// escapes, and nested subtraction. The subtraction right-hand side is
    /// itself a character class, so this recurses and is depth-bounded exactly
    /// like [`Parser::parse_group`].
    fn parse_class(&mut self) -> Result<ClassExpr, RegexError> {
        self.enter_nesting()?;
        let result = self.parse_class_inner();
        self.leave_nesting();
        result
    }

    /// The body of [`Parser::parse_class`], called with the nesting level
    /// already accounted for.
    fn parse_class_inner(&mut self) -> Result<ClassExpr, RegexError> {
        self.bump(); // '['
        let negated = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut items = Vec::new();
        let mut subtract = None;
        loop {
            match self.peek() {
                None => {
                    return Err(RegexError::InvalidPattern(
                        "unterminated character class".to_owned(),
                    ));
                }
                Some(']') => {
                    self.bump();
                    break;
                }
                // `charClassSub ::= ( posCharGroup | negCharGroup ) '-' charClassExpr`
                // (XSD Appendix G.3): the left-hand side of a subtraction is a
                // `posCharGroup`, defined as `( charOrEsc | ... )+` — one or
                // more members, never empty. So `-[` only *starts* a
                // subtraction once at least one member has already been
                // parsed; a `-[` with nothing before it (e.g. `[-[bc]]` or
                // `[^-[bc]]`) instead falls through to the default arm below,
                // where the leading '-' becomes an ordinary literal member and
                // the following unescaped '[' is then correctly rejected.
                // Confirmed invalid by QT3 `re00754`/`re00988`/`re00989` and
                // the XML Schema Test Suite's `RegexTest_469`/`RegexTest_470`
                // (all `[^-[bc]]` or `[-[xyz]]`/`[^-[xyz]]`).
                Some('-') if self.peek_at(1) == Some('[') && !items.is_empty() => {
                    self.bump(); // '-'
                    subtract = Some(Box::new(self.parse_class()?));
                    if self.peek() != Some(']') {
                        return Err(RegexError::InvalidPattern(
                            "expected ']' after class subtraction".to_owned(),
                        ));
                    }
                    self.bump();
                    break;
                }
                _ => {
                    // Whether the member about to be parsed is a *raw*,
                    // unescaped '-': captured before consuming it, because
                    // `parse_class_member` returns the same `Member::Char('-')`
                    // for both a literal `-` and the escape `\-`, and only the
                    // raw form is disqualified as a range endpoint below.
                    let c_is_raw_hyphen = self.peek() == Some('-');
                    let member = self.parse_class_member()?;
                    match member {
                        Member::Char(c) => {
                            let is_range = self.peek() == Some('-')
                                && self.peek_at(1) != Some(']')
                                && self.peek_at(1) != Some('[')
                                && self.peek_at(1).is_some();
                            if is_range {
                                // `seRange ::= charOrEsc '-' charOrEsc` (XSD
                                // Appendix G.3 rule 16 / XPath F&O 3.1
                                // §5.6.1), where `charOrEsc ::= XmlChar |
                                // SingleCharEsc` and `XmlChar` is `Char`
                                // *minus* '-', '[', and ']' (rule 18). A raw,
                                // unescaped hyphen therefore never satisfies
                                // `charOrEsc` and can never be either
                                // endpoint of a range -- only the escape `\-`
                                // can (it satisfies `SingleCharEsc` instead).
                                // A raw hyphen can still be an ordinary
                                // standalone member elsewhere in the class
                                // (the separate `XmlCharIncDash` production,
                                // rule 15), which is why this check only
                                // fires once `is_range` has already committed
                                // to a range attempt -- a lone `-` that
                                // cannot attach to anything (e.g. in
                                // `[a-d-b-c]` or `[a-]`) is unaffected, since
                                // `is_range` is false for it.
                                if c_is_raw_hyphen {
                                    return Err(RegexError::InvalidPattern(
                                        "an unescaped '-' may not begin a character range; \
                                         write \\- to use a literal hyphen as a range endpoint"
                                            .to_owned(),
                                    ));
                                }
                                self.bump(); // '-'
                                let end_is_raw_hyphen = self.peek() == Some('-');
                                let end = match self.parse_class_member()? {
                                    Member::Char(e) => e,
                                    Member::Item(_) => {
                                        return Err(RegexError::InvalidPattern(
                                            "class escape may not be a range endpoint".to_owned(),
                                        ));
                                    }
                                };
                                // The pre-existing ordering check below
                                // already happens to reject some raw-hyphen
                                // ends by coincidence (a raw '-' end sorts as
                                // U+002D, which is out of order whenever `c`
                                // is anything past it, e.g. `[a--b]`) -- keep
                                // deferring to it first, unchanged, wherever
                                // it already independently catches the case,
                                // and only fall back to this newer, more
                                // precise check for the rest (e.g. `[!--]`,
                                // where '!' sorts *before* U+002D so the
                                // ordering check alone would wrongly accept
                                // it). Same exclusion as the start check
                                // above, applied to the other endpoint. XSTS
                                // `saxonData/Simple/simple041` (`[--z]*`) and
                                // `simple042` (`[!--]*`), both `<expected
                                // validity="invalid"/>`, and QT3
                                // `fn-matches.re` `re00071` (`[a--b]`,
                                // unconditionally `FORX0002` with no
                                // XSD-version `<dependency>`) and the XML
                                // Schema Test Suite's own `msData/regex/reF57`
                                // (`[a--b]`, `<expected validity="invalid"/>`,
                                // annotated `RULE='15,16'`) all confirm a
                                // second, immediately-adjacent raw hyphen
                                // right after a range operator is rejected --
                                // never silently treated as "the range end
                                // happens to be U+002D". (`reF56`,
                                // `[a-z--[b-z]]`, stays valid: there the
                                // second hyphen is immediately followed by
                                // '[' and starts a class subtraction instead,
                                // which is excluded from `is_range` above
                                // before this check is ever reached.)
                                if (c as u32) > (end as u32) {
                                    return Err(RegexError::InvalidPattern(
                                        "character range is out of order".to_owned(),
                                    ));
                                }
                                if end_is_raw_hyphen {
                                    return Err(RegexError::InvalidPattern(
                                        "an unescaped '-' may not end a character range; \
                                         write \\- to use a literal hyphen as a range endpoint"
                                            .to_owned(),
                                    ));
                                }
                                items.push(ClassItem::Range(c, end));
                            } else {
                                if c_is_raw_hyphen && self.is_xsd_1_0() {
                                    self.check_hyphen_member_position_1_0(&items)?;
                                }
                                items.push(ClassItem::Single(c));
                            }
                        }
                        Member::Item(it) => items.push(it),
                    }
                }
            }
        }
        if items.is_empty() && subtract.is_none() {
            return Err(RegexError::InvalidPattern(
                "empty character class".to_owned(),
            ));
        }
        Ok(ClassExpr {
            negated,
            items,
            subtract,
        })
    }

    /// Enforces the XSD 1.0 restriction on where a raw, unescaped `-` may
    /// stand as a member of its own inside a character class.
    ///
    /// Both grammars derive a bare hyphen through the same production
    /// (`XmlCharIncDash`, XSD 1.0 Appendix F rule 19 / XSD 1.1's
    /// `SingleCharNoEsc`), but XSD 1.0 confines it to the first or the last
    /// member of the positive character group it belongs to, so a hyphen may
    /// separate a group's members from its `]` or from a class subtraction but
    /// never sit *between* two other members. XSD 1.1 dropped that restriction:
    /// a hyphen that cannot attach to a range simply becomes an ordinary
    /// member wherever it stands.
    ///
    /// The three shapes this rejects are exactly the ones the two W3C suites
    /// split by version, in matched pairs that pin both directions:
    ///
    /// * `[^a-d-b-c]` — XML Schema Test Suite `msData/regex/reF20`-`reF23`,
    ///   each `<expected version="1.0" validity="invalid"/>` plus `<expected
    ///   version="1.1" validity="valid"/>`; QT3 `fn-matches.re` `re00056`
    ///   (`<dependency type="xsd-version" value="1.0"/>`, `FORX0002`) against
    ///   `re00056a` (`value="1.1"`, succeeds).
    /// * `[a-c-1-4x-z-7-9]*` — `reG26`-`reG33`; QT3 `re00086`/`re00086a`.
    /// * `[a-a-x-x]+` — `reH19`-`reH21`; QT3 `re00102a`/`re00102`.
    ///
    /// Both suites record the same cause: QT3's `<modified … change="split
    /// into XSD 1.0 and XSD 1.1 tests as per bug 21425"/>`.
    ///
    /// The hyphens that stay legal in *both* versions are the ones at an end
    /// of the group: `[-]`, `[-a]+`, `[a-]*` (`reH16`/`reH17`/`reH18`),
    /// `[^-z]+` (`reF14`), `[-e-g]+` (`RegexTest_466`), and — the case that
    /// makes "last member" mean "last before the subtraction operator", not
    /// just "last before `]`" — `[a-z--[b-z]]` (`reF56`), where the group is
    /// `a-z` plus a literal `-`, and the *second* hyphen opens the
    /// subtraction. All of those carry a single, unconditional `<expected
    /// validity="valid"/>`.
    fn check_hyphen_member_position_1_0(&self, items: &[ClassItem]) -> Result<(), RegexError> {
        // First member of the (positive) character group: nothing has been
        // parsed yet, whether or not the group opened with `^`.
        if items.is_empty() {
            return Ok(());
        }
        // Last member: the group ends here, either at its own `]` or at the
        // `-[` that starts a class subtraction.
        let ends_group =
            self.peek() == Some(']') || (self.peek() == Some('-') && self.peek_at(1) == Some('['));
        if ends_group {
            return Ok(());
        }
        Err(RegexError::InvalidPattern(
            "an unescaped '-' may only be the first or the last member of a character group \
             under XSD 1.0; write \\- for a literal hyphen elsewhere"
                .to_owned(),
        ))
    }

    /// Parses one member inside a character class.
    fn parse_class_member(&mut self) -> Result<Member, RegexError> {
        self.budget.charge_work(1)?;
        match self.peek() {
            None => Err(RegexError::InvalidPattern(
                "unterminated character class".to_owned(),
            )),
            Some('\\') => {
                self.bump();
                let n = match self.peek() {
                    Some(c) => c,
                    None => {
                        return Err(RegexError::InvalidPattern(
                            "character class ends with a backslash".to_owned(),
                        ));
                    }
                };
                if let Some(ch) = self.single_char_escape(n) {
                    self.bump();
                    return Ok(Member::Char(ch));
                }
                if let Some(sh) = shorthand_escape(n) {
                    self.bump();
                    return Ok(Member::Item(ClassItem::Shorthand(sh)));
                }
                if n == 'p' || n == 'P' {
                    let (name, negated) = self.parse_property()?;
                    return Ok(Member::Item(ClassItem::Prop { name, negated }));
                }
                if n.is_ascii_digit() {
                    return Err(RegexError::InvalidPattern(
                        "a backslash followed by a digit is not allowed in a character class"
                            .to_owned(),
                    ));
                }
                Err(RegexError::InvalidPattern(format!(
                    "unrecognised escape \\{n} in character class"
                )))
            }
            Some('[') => Err(RegexError::InvalidPattern(
                "'[' must be escaped inside a character class".to_owned(),
            )),
            Some(c) => {
                self.bump();
                Ok(Member::Char(c))
            }
        }
    }
}

/// A member parsed inside a character class: either a rangeable single
/// character, or a class escape that may not participate in a range.
enum Member {
    Char(char),
    Item(ClassItem),
}

/// Maps a multi-character escape letter to its [`Shorthand`].
fn shorthand_escape(n: char) -> Option<Shorthand> {
    Some(match n {
        's' => Shorthand::Space,
        'S' => Shorthand::NotSpace,
        'i' => Shorthand::NameStart,
        'I' => Shorthand::NotNameStart,
        'c' => Shorthand::NameChar,
        'C' => Shorthand::NotNameChar,
        'd' => Shorthand::Digit,
        'D' => Shorthand::NotDigit,
        'w' => Shorthand::Word,
        'W' => Shorthand::NotWord,
        _ => return None,
    })
}

/// Interprets the text inside a `\p{...}` escape as a group, category, or
/// block, for the given `dialect` and XSD `version`.
fn interpret_property_name(
    name: &str,
    dialect: Dialect,
    version: XsdVersion,
    budget: &mut Budget<'_>,
) -> Result<PropName, RegexError> {
    if name.is_empty() {
        return Err(RegexError::InvalidPattern(
            "empty \\p{} property name".to_owned(),
        ));
    }
    if let Some(block) = name.strip_prefix("Is") {
        // `IsBlock ::= 'Is' [a-zA-Z0-9#x2D]+` (XSD Appendix G.4.2.3): the
        // empty string after "Is" never matches this production and is
        // always invalid, in both dialects (XML Schema Test Suite
        // `msData/regex/reK87`, `\P{Is}`, unconditionally `invalid`).
        if block.is_empty()
            || !block
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(RegexError::InvalidPattern(
                "Unicode block name must be nonempty ASCII letters, digits, or hyphens".to_owned(),
            ));
        }
        if let Some(resolved) = resolve_block_bounded(block, budget)? {
            return Ok(resolved);
        }
        // A non-empty run of characters after "Is" is syntactically a valid
        // `IsBlock`, even when (as here) it does not name any block this
        // engine recognises. XSD 1.1 changed the rule from XSD 1.0: such a
        // pattern is no longer a schema error, it matches every character
        // ("most of these strings ... will not denote any Unicode block ...
        // Processors may at user option treat unrecognized block names as
        // errors" — W3C bug 13670). XSD 1.0 keeps the older, strict reading,
        // and the XML Schema Test Suite pins both directions on one pattern:
        // `msData/regex/reK88` (`\p{IsaA0-a9}`) carries `<expected
        // version="1.0" validity="invalid"/>` alongside `<expected
        // version="1.1" validity="valid"/>`. The XPath/F&O grammar does not
        // carry the 1.1 leniency at all: QT3 `fn-matches.re` `re00216` (the
        // very same pattern) expects `FORX0002` there unconditionally.
        if dialect == Dialect::Xsd && version == XsdVersion::V1_1 {
            return Ok(PropName::UnknownBlock);
        }
        return Err(RegexError::InvalidPattern(format!(
            "unknown Unicode block name {name:?}"
        )));
    }
    let mut it = name.chars();
    let first = it.next().unwrap_or(' ');
    if name.len() == 1 && matches!(first, 'L' | 'M' | 'N' | 'P' | 'S' | 'Z' | 'C') {
        return Ok(PropName::Group(first));
    }
    if let Some(cat) = category_from_abbreviation(name) {
        return Ok(PropName::Category(cat));
    }
    Err(RegexError::InvalidPattern(format!(
        "unknown Unicode property name {name:?}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(
        pattern: &str,
        dialect: Dialect,
        version: XsdVersion,
        flags: Flags,
    ) -> Result<Parsed, RegexError> {
        let limits = crate::budget::Limits {
            captures: 200_000,
            ..Default::default()
        };
        super::parse(pattern, dialect, version, flags, &mut Budget::new(limits))
    }

    fn strip(pattern: &str) -> String {
        strip_whitespace(pattern).into_iter().collect()
    }

    #[test]
    fn whitespace_stripping_respects_classes_and_escapes() {
        assert_eq!(strip("a b\tc"), "abc");
        // Whitespace outside the class is dropped, inside the class is kept.
        assert_eq!(strip("a [b c]"), "a[b c]");
        assert_eq!(strip("[b c]"), "[b c]");
        // A backslash does not protect whitespace: F&O 3.1 §5.6.1 removes it
        // anyway, so the backslash binds to the next non-whitespace character
        // (`hello\ sworld` is `hello\sworld`).
        assert_eq!(strip("a\\ b"), "a\\b");
        assert_eq!(strip("hello\\ sworld"), "hello\\sworld");
        // A normal escape is preserved verbatim.
        assert_eq!(strip("a\\.b"), "a\\.b");
        // Inside a class nothing is removed, escape or not.
        assert_eq!(strip("[a\\ b]"), "[a\\ b]");
    }

    #[test]
    fn capturing_group_count() {
        let chars: Vec<char> = "(a)(?:b)(c)".chars().collect();
        assert_eq!(count_capturing_groups(&chars, Dialect::Xpath), 2);
        // In XSD every '(' is capturing.
        assert_eq!(count_capturing_groups(&chars, Dialect::Xsd), 3);
        // Parentheses inside a class do not count.
        let chars: Vec<char> = "[()]a".chars().collect();
        assert_eq!(count_capturing_groups(&chars, Dialect::Xpath), 0);
    }

    /// Regression: pattern *compilation* is recursive descent, so a deeply
    /// parenthesised pattern used to abort the process with a stack overflow
    /// (uncatchable, i.e. a denial of service reachable from any untrusted
    /// `xs:pattern` facet or `fn:matches` argument). It must now be reported
    /// as a typed resource error instead.
    #[test]
    fn deeply_nested_groups_are_rejected_not_overflowed() {
        for dialect in [Dialect::Xsd, Dialect::Xpath] {
            let depth = 100_000;
            let pattern = format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
            let Err(error) = parse(&pattern, dialect, XsdVersion::V1_1, Flags::default()) else {
                panic!("{dialect:?}: a 100000-deep pattern must be rejected");
            };
            assert!(
                matches!(&error, RegexError::ResourceLimit { .. }),
                "{dialect:?}: unexpected error {error:?}"
            );
        }
    }

    /// The same ceiling applies to the other recursive production, the XSD
    /// character-class subtraction `[a-[b-[c-[...]]]]`.
    #[test]
    fn deeply_nested_class_subtractions_are_rejected_not_overflowed() {
        let depth = 50_000;
        let pattern = format!("{}a{}", "[a-".repeat(depth), "]".repeat(depth));
        let Err(error) = parse(&pattern, Dialect::Xsd, XsdVersion::V1_1, Flags::default()) else {
            panic!("a 50000-deep class subtraction must be rejected");
        };
        assert!(
            matches!(&error, RegexError::ResourceLimit { .. }),
            "unexpected error {error:?}"
        );
    }

    /// The ceiling must not reject anything a real pattern could plausibly
    /// contain, and the depth counter must be *decremented* on the way out so
    /// that many shallow groups in sequence stay acceptable.
    #[test]
    fn nesting_below_the_ceiling_still_parses() {
        let depth = crate::budget::Limits::default().nesting;
        let nested = format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
        assert!(parse(&nested, Dialect::Xpath, XsdVersion::V1_1, Flags::default()).is_ok());

        // Depth is per-branch, not cumulative: 10000 sibling groups nest only
        // one level deep and must still be accepted.
        let siblings = "(a)".repeat(10_000);
        assert!(
            parse(
                &siblings,
                Dialect::Xpath,
                XsdVersion::V1_1,
                Flags::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn backreference_digits_use_groups_opened_at_that_position() {
        let parsed = parse(
            r"^(a)\10(b)(c)(d)(e)(f)(g)(h)(i)(j)$",
            Dialect::Xpath,
            XsdVersion::V1_1,
            Flags::default(),
        )
        .expect("the back-reference digit-prefix fixture must parse");
        let Ast::Concat(items) = parsed.ast else {
            panic!("expected sequence")
        };
        assert!(matches!(items[2], Ast::Backref(1)));
        assert!(matches!(items[3], Ast::Literal('0')));
        for invalid in [r"\1(a)", r"((#)abc\1)$"] {
            assert!(matches!(
                parse(invalid, Dialect::Xpath, XsdVersion::V1_1, Flags::default()),
                Err(RegexError::InvalidPattern(_))
            ));
        }
    }

    #[test]
    fn nested_class_whitespace_and_block_lexical_rules_are_preserved() {
        let flags = Flags {
            ignore_whitespace: true,
            ..Flags::default()
        };
        assert!(matches!(
            parse("[a-[b] ]", Dialect::Xpath, XsdVersion::V1_1, flags),
            Err(RegexError::InvalidPattern(_))
        ));
        assert!(parse("[a-[b]] ", Dialect::Xpath, XsdVersion::V1_1, flags).is_ok());
        for invalid in [r"\p{IsPrivate_Use}", r"\p{IsPrivate Use}"] {
            assert!(matches!(
                parse(invalid, Dialect::Xpath, XsdVersion::V1_1, Flags::default()),
                Err(RegexError::InvalidPattern(_))
            ));
        }
        assert!(
            parse(
                r"\p{IsPrivateUse}",
                Dialect::Xpath,
                XsdVersion::V1_1,
                Flags::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn flags_reject_unknown() {
        assert!(Flags::parse("smixq").is_ok());
        assert!(matches!(
            Flags::parse("z"),
            Err(RegexError::InvalidFlags(_))
        ));
    }
}
