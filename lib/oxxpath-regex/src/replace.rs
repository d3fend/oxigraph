//! XPath replacement handling, retaining D3FEND's checked linear numeric
//! capture-prefix parser and validating before any input matching.
use crate::budget::Budget;
use crate::{RegexError, allocation_error};

#[derive(Debug)]
enum Part<'a> {
    Literal(&'a str),
    Character(char),
    Capture(usize),
}

pub(crate) struct Template<'a> {
    parts: Vec<Part<'a>>,
}

impl<'a> Template<'a> {
    pub(crate) fn parse(
        replacement: &'a str,
        captures: usize,
        literal: bool,
        budget: &mut Budget<'_>,
    ) -> Result<Self, RegexError> {
        budget.check_output(replacement.len())?;
        budget.charge_work(replacement.len())?;
        // Each part consumes at least one source byte. One bounded allocation
        // avoids repeated growth and keeps parsing independent of match count.
        let capacity = if literal { 1 } else { replacement.len() };
        budget.charge_alloc(
            capacity
                .checked_mul(std::mem::size_of::<Part<'_>>())
                .ok_or_else(|| allocation_error(budget))?,
        )?;
        let mut parts = Vec::new();
        parts
            .try_reserve_exact(capacity)
            .map_err(|_| allocation_error(budget))?;
        if literal {
            parts.push(Part::Literal(replacement));
            return Ok(Self { parts });
        }
        let bytes = replacement.as_bytes();
        let mut offset = 0;
        while offset < bytes.len() {
            match bytes[offset] {
                b'\\' => {
                    match bytes.get(offset + 1) {
                        Some(b'\\') => parts.push(Part::Character('\\')),
                        Some(b'$') => parts.push(Part::Character('$')),
                        _ => {
                            return Err(RegexError::InvalidReplacement(
                                "a backslash must escape a dollar sign or backslash".into(),
                            ));
                        }
                    }
                    offset += 2;
                }
                b'$' => {
                    offset += 1;
                    if !bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                        return Err(RegexError::InvalidReplacement(
                            "a dollar sign must be followed by a digit".into(),
                        ));
                    }
                    let mut capture = 0usize;
                    while let Some(digit) = bytes.get(offset).filter(|b| b.is_ascii_digit()) {
                        let Some(next) = capture
                            .checked_mul(10)
                            .and_then(|value| value.checked_add(usize::from(*digit - b'0')))
                            .filter(|value| *value <= captures.max(9))
                        else {
                            break;
                        };
                        capture = next;
                        offset += 1;
                    }
                    parts.push(Part::Capture(capture));
                    let start = offset;
                    while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                        offset += 1;
                    }
                    if start != offset {
                        parts.push(Part::Literal(&replacement[start..offset]));
                    }
                }
                _ => {
                    let start = offset;
                    while offset < bytes.len() && !matches!(bytes[offset], b'\\' | b'$') {
                        offset += 1;
                    }
                    parts.push(Part::Literal(&replacement[start..offset]));
                }
            }
        }
        Ok(Self { parts })
    }

    pub(crate) fn expand_into(
        &self,
        out: &mut String,
        input: &[char],
        caps: &[Option<usize>],
        budget: &mut Budget<'_>,
    ) -> Result<(), RegexError> {
        for part in &self.parts {
            budget.charge_work(1)?;
            match part {
                Part::Literal(text) => append_str(out, text, budget)?,
                Part::Character(c) => {
                    let mut encoded = [0u8; 4];
                    append_str(out, c.encode_utf8(&mut encoded), budget)?;
                }
                Part::Capture(group) => {
                    let start = caps.get(2 * group).copied().flatten();
                    let end = caps.get(2 * group + 1).copied().flatten();
                    if let (Some(start), Some(end)) = (start, end) {
                        append_chars(out, &input[start..end], budget)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn reserve_output(
    out: &mut String,
    additional: usize,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let needed = out
        .len()
        .checked_add(additional)
        .ok_or_else(|| allocation_error(budget))?;
    budget.check_output(needed)?;
    if needed > out.capacity() {
        let capacity = needed
            .max(out.capacity().saturating_mul(2).max(64))
            .min(budget.limits.output_bytes);
        // Cumulative requested allocation, not an allocator/RSS measurement.
        budget.charge_alloc(capacity)?;
        budget.charge_work(out.len())?;
        out.try_reserve_exact(capacity - out.len())
            .map_err(|_| allocation_error(budget))?;
    }
    Ok(())
}

pub(crate) fn append_str(
    out: &mut String,
    text: &str,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.charge_work(text.len())?;
    reserve_output(out, text.len(), budget)?;
    out.push_str(text);
    Ok(())
}

pub(crate) fn append_chars(
    out: &mut String,
    chars: &[char],
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.charge_work(chars.len())?;
    let bytes = chars
        .iter()
        .try_fold(0usize, |n, c| n.checked_add(c.len_utf8()))
        .ok_or_else(|| allocation_error(budget))?;
    reserve_output(out, bytes, budget)?;
    budget.charge_work(bytes)?;
    out.extend(chars.iter());
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{Limits, Regex, RegexError, XsdVersion};

    #[test]
    fn replacement_validation_is_independent_of_matches() {
        let regex = Regex::xpath_versioned("a", "", XsdVersion::V1_1)
            .expect("literal pattern must compile");
        for replacement in ["$", "$x", "\\x", "ends\\"] {
            assert!(matches!(
                regex.replace("z", replacement),
                Err(RegexError::InvalidReplacement(_))
            ));
        }
        assert_eq!(
            regex
                .replace("cat", r"\$\\")
                .expect("valid replacement must complete within default limits"),
            "c$\\t"
        );
    }

    #[test]
    fn numeric_capture_prefix_and_nonparticipating_groups() {
        let regex = Regex::xpath_versioned("(a)(b)?", "", XsdVersion::V1_1)
            .expect("optional capturing pattern must compile");
        assert_eq!(
            regex
                .replace("a", "$0/$23/$01/$9")
                .expect("valid replacement must complete within default limits"),
            "a/3/a/"
        );
        let digits = "9".repeat(1024);
        assert_eq!(
            regex
                .replace("a", &format!("${digits}"))
                .expect("valid replacement must complete within default limits"),
            digits[1..]
        );
        assert_eq!(
            regex
                .replace("a", "$00000000000000000000001")
                .expect("valid replacement must complete within default limits"),
            "a"
        );
    }

    #[test]
    fn output_limit_is_fatal_and_quoted_replacement_is_literal() {
        let regex = Regex::xpath_versioned("a", "q", XsdVersion::V1_1)
            .expect("quoted literal pattern must compile");
        assert_eq!(
            regex
                .replace("aa", "$0")
                .expect("valid replacement must complete within default limits"),
            "$0$0"
        );
        let limits = Limits {
            output_bytes: 3,
            ..Limits::default()
        };
        assert!(matches!(
            regex.replace_with_limits("aa", "$0", limits),
            Err(RegexError::ResourceLimit { .. })
        ));
    }
}
