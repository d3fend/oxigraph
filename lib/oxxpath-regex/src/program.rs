//! Compilation of a back-reference-free AST to a Pike-VM program.
//!
//! The instruction set is the classic Thompson/Pike design: character
//! consumers, unconditional jumps, priority-ordered splits, capture saves, and
//! zero-width anchor assertions. Repetitions are expanded into splits and
//! jumps; counted repetitions are unrolled with a guard against pathological
//! expansion sizes. Programs produced here never contain a back-reference —
//! patterns that need one are routed to the back-tracking interpreter instead.

use std::rc::Rc;

use crate::ast::Ast;
use crate::budget::Budget;
use crate::charclass::{ClassExpr, any_variant_bounded};
use crate::error::RegexError;

/// A single-character matcher stored inside a [`Inst::Char`] instruction.
#[derive(Debug, Clone)]
pub(crate) enum Matcher {
    /// Matches exactly one literal character (case-folded when requested).
    Literal(char),
    /// Matches a character class.
    Class(Rc<ClassExpr>),
}

impl Matcher {
    /// Tests a character, charging class traversal and Unicode variants.
    pub(crate) fn matches(
        &self,
        c: char,
        ci: bool,
        budget: &mut Budget<'_>,
    ) -> Result<bool, RegexError> {
        match self {
            Matcher::Literal(l) => {
                if ci {
                    any_variant_bounded(c, |v| v == *l, budget)
                } else {
                    budget.charge_work(1)?;
                    Ok(c == *l)
                }
            }
            Matcher::Class(cl) => cl.contains_bounded(c, ci, budget),
        }
    }
}

/// A Pike-VM instruction.
#[derive(Debug, Clone)]
pub(crate) enum Inst {
    /// Consume one input character if it matches, then advance.
    Char(Matcher),
    /// Accept the match.
    Match,
    /// Continue at the given instruction index.
    Jmp(usize),
    /// Try the first target with priority, the second on back-track.
    Split(usize, usize),
    /// Record the current input position into a capture slot.
    Save(usize),
    /// Zero-width assertion: succeed only at the start of the string (or of a
    /// line, in multi-line mode).
    AssertStart,
    /// Zero-width assertion: succeed only at the end of the string (or of a
    /// line, in multi-line mode).
    AssertEnd,
}

/// A compiled program together with its capture-group count.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    /// The instruction sequence.
    pub insts: Vec<Inst>,
    /// The number of capturing groups (excluding the whole-match group 0).
    pub group_count: usize,
}

/// Compiles `ast` into a Pike-VM [`Program`]. The whole match is bracketed with
/// `Save(0)` / `Save(1)`.
pub(crate) fn compile(
    ast: &Ast,
    group_count: usize,
    budget: &mut Budget<'_>,
) -> Result<Program, RegexError> {
    budget.check_captures(group_count)?;
    let mut insts = Vec::new();
    push_inst(&mut insts, Inst::Save(0), budget)?;
    emit(ast, &mut insts, budget)?;
    push_inst(&mut insts, Inst::Save(1), budget)?;
    push_inst(&mut insts, Inst::Match, budget)?;
    Ok(Program { insts, group_count })
}

fn push_inst(insts: &mut Vec<Inst>, inst: Inst, budget: &mut Budget<'_>) -> Result<(), RegexError> {
    budget.check_instructions(insts.len().saturating_add(1))?;
    budget.charge_work(3)?;
    // Includes the cumulative allocation/copy envelope of geometric Vec growth.
    budget.charge_alloc(4 * std::mem::size_of::<Inst>())?;
    insts.push(inst);
    Ok(())
}

fn emit(node: &Ast, insts: &mut Vec<Inst>, budget: &mut Budget<'_>) -> Result<(), RegexError> {
    budget.charge_work(1)?;
    match node {
        Ast::Empty => {}
        Ast::Literal(c) => push_inst(insts, Inst::Char(Matcher::Literal(*c)), budget)?,
        Ast::Class(cl) => {
            budget.charge_alloc(2 * std::mem::size_of::<usize>())?;
            push_inst(
                insts,
                Inst::Char(Matcher::Class(Rc::new(cl.budgeted_clone(budget)?))),
                budget,
            )?;
        }
        Ast::AnchorStart => push_inst(insts, Inst::AssertStart, budget)?,
        Ast::AnchorEnd => push_inst(insts, Inst::AssertEnd, budget)?,
        Ast::Concat(items) => {
            for item in items {
                emit(item, insts, budget)?;
            }
        }
        Ast::Alt(branches) => emit_alt(branches, insts, budget)?,
        Ast::Group { index, inner } => {
            if let Some(i) = index {
                push_inst(insts, Inst::Save(2 * i), budget)?;
            }
            emit(inner, insts, budget)?;
            if let Some(i) = index {
                push_inst(insts, Inst::Save(2 * i + 1), budget)?;
            }
        }
        Ast::Repeat {
            inner,
            min,
            max,
            greedy,
        } => emit_repeat(inner, *min, *max, *greedy, insts, budget)?,
        Ast::Backref(_) => {
            return Err(RegexError::InvalidPattern(
                "internal error: back-reference reached the Pike compiler".to_owned(),
            ));
        }
    }
    Ok(())
}

fn emit_alt(
    branches: &[Ast],
    insts: &mut Vec<Inst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let mut jmp_positions = Vec::new();
    let last = branches.len().saturating_sub(1);
    for (i, branch) in branches.iter().enumerate() {
        if i < last {
            let split_pos = insts.len();
            push_inst(insts, Inst::Split(0, 0), budget)?;
            emit(branch, insts, budget)?;
            let jmp_pos = insts.len();
            push_inst(insts, Inst::Jmp(0), budget)?;
            budget.charge_alloc(4 * std::mem::size_of::<usize>())?;
            jmp_positions.push(jmp_pos);
            let next = insts.len();
            insts[split_pos] = Inst::Split(split_pos + 1, next);
        } else {
            emit(branch, insts, budget)?;
        }
    }
    let end = insts.len();
    for pos in jmp_positions {
        insts[pos] = Inst::Jmp(end);
    }
    Ok(())
}

fn emit_repeat(
    inner: &Ast,
    min: u32,
    max: Option<u32>,
    greedy: bool,
    insts: &mut Vec<Inst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let min = min as usize;
    budget.check_instructions(min)?;
    for _ in 0..min {
        emit(inner, insts, budget)?;
        budget.charge_work(1)?;
    }
    match max {
        None => emit_star(inner, greedy, insts, budget)?,
        Some(max) => {
            let max = max as usize;
            let optional = max.saturating_sub(min);
            budget.check_instructions(optional)?;
            emit_optionals(inner, optional, greedy, insts, budget)?;
        }
    }
    Ok(())
}

/// Emits `inner*` (or `inner*?` when not greedy).
fn emit_star(
    inner: &Ast,
    greedy: bool,
    insts: &mut Vec<Inst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let split_pos = insts.len();
    push_inst(insts, Inst::Split(0, 0), budget)?;
    emit(inner, insts, budget)?;
    push_inst(insts, Inst::Jmp(split_pos), budget)?;
    let after = insts.len();
    insts[split_pos] = if greedy {
        Inst::Split(split_pos + 1, after)
    } else {
        Inst::Split(after, split_pos + 1)
    };
    Ok(())
}

/// Emits `count` nested optional copies of `inner`, giving `inner{0,count}`.
fn emit_optionals(
    inner: &Ast,
    count: usize,
    greedy: bool,
    insts: &mut Vec<Inst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.charge_alloc_product(count, 4 * std::mem::size_of::<usize>())?;
    let mut splits = Vec::new();
    for _ in 0..count {
        budget.charge_work(1)?;
        let split_pos = insts.len();
        push_inst(insts, Inst::Split(0, 0), budget)?;
        splits.push(split_pos);
        emit(inner, insts, budget)?;
    }
    let after = insts.len();
    for split_pos in splits {
        budget.charge_work(1)?;
        insts[split_pos] = if greedy {
            Inst::Split(split_pos + 1, after)
        } else {
            Inst::Split(after, split_pos + 1)
        };
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::XsdVersion;
    use crate::budget::Limits;
    use crate::parser::{self, Dialect, Flags};

    #[test]
    fn optional_repetition_compilation_is_iterative_and_bounded() {
        let mut budget = Budget::new(Limits::default());
        let parsed = parser::parse(
            "a{0,1000}",
            Dialect::Xpath,
            XsdVersion::V1_1,
            Flags::default(),
            &mut budget,
        )
        .expect("one thousand optional copies must fit parser limits");
        let program = compile(&parsed.ast, parsed.group_count, &mut budget)
            .expect("one thousand optional copies must fit default instruction limits");
        assert_eq!(program.insts.len(), 2003);
        let mut budget = Budget::new(Limits {
            instructions: 2002,
            ..Limits::default()
        });
        assert!(matches!(
            compile(&parsed.ast, parsed.group_count, &mut budget),
            Err(RegexError::ResourceLimit {
                stage: "instructions",
                limit: 2002
            })
        ));
    }

    #[test]
    fn oversized_repeats_never_compile_to_a_false_answer() {
        for pattern in ["a{200001}|b{200002}", "(?:){200001}"] {
            let mut budget = Budget::new(Limits::default());
            let parsed = parser::parse(
                pattern,
                Dialect::Xpath,
                XsdVersion::V1_1,
                Flags::default(),
                &mut budget,
            )
            .expect("oversized repeats are syntactically valid before compilation");
            assert!(matches!(
                compile(&parsed.ast, parsed.group_count, &mut budget),
                Err(RegexError::ResourceLimit { .. })
            ));
        }
    }
}
