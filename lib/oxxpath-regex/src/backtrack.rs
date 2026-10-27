//! A back-tracking interpreter for patterns that use back-references.
//!
//! Back-references make a pattern non-regular, so the linear-time Pike VM cannot
//! evaluate them. This module compiles such a pattern to a small instruction set
//! and runs it with an explicit heap-allocated back-track stack (the native call
//! stack stays at constant depth). Two safeguards keep pathological patterns
//! from hanging:
//!
//! * every executed step, at every position a search tries, is counted against
//!   a single budget the caller threads through for the duration of one
//!   logical operation (see [`find_from`]); exhausting it returns
//!   [`RegexError::ResourceLimit`] instead of looping forever. This bounds
//!   the *total* work done, not just the worst single starting position: a
//!   single position that back-tracks catastrophically (for example
//!   `(a+)+\1$` against a long non-matching string) exhausts it immediately,
//!   and so, eventually, does a pattern that is merely expensive -- never
//!   catastrophic -- at each of unboundedly many positions;
//! * unbounded repetitions carry a loop guard ([`BtInst::MarkLoop`] /
//!   [`BtInst::LoopBack`]) so a body that matches the empty string cannot spin.

use std::rc::Rc;

use crate::ast::Ast;
use crate::budget::Budget;
use crate::charclass::{ClassExpr, any_variant_bounded};
use crate::error::RegexError;
use crate::pikevm::{Captures, at_end, at_start};

/// A back-tracking instruction.
#[derive(Debug, Clone)]
pub(crate) enum BtInst {
    /// Consume one input character if it matches a literal.
    Literal(char),
    /// Consume one input character if it matches a class.
    Class(Rc<ClassExpr>),
    /// Accept the match.
    Match,
    /// Continue at the given instruction.
    Jmp(usize),
    /// Try the first target, keeping the second for back-tracking.
    Split(usize, usize),
    /// Record the current position into a capture slot.
    Save(usize),
    /// Match the text previously captured by a group (empty if it did not
    /// participate).
    Backref(usize),
    /// Zero-width start assertion.
    AssertStart,
    /// Zero-width end assertion.
    AssertEnd,
    /// Push the current position as the entry point of a repetition.
    MarkLoop,
    /// Pop a loop entry; jump to the first target if the position advanced,
    /// otherwise to the second (the loop exit).
    LoopBack(usize, usize),
}

/// A compiled back-tracking program.
#[derive(Debug, Clone)]
pub(crate) struct BtProgram {
    /// The instruction sequence.
    pub insts: Vec<BtInst>,
    /// The number of capturing groups (excluding the whole-match group 0).
    pub group_count: usize,
}

/// Compiles `ast` into a back-tracking [`BtProgram`].
pub(crate) fn compile(
    ast: &Ast,
    group_count: usize,
    budget: &mut Budget<'_>,
) -> Result<BtProgram, RegexError> {
    budget.check_captures(group_count)?;
    let mut insts = Vec::new();
    push_inst(&mut insts, BtInst::Save(0), budget)?;
    emit(ast, &mut insts, budget)?;
    push_inst(&mut insts, BtInst::Save(1), budget)?;
    push_inst(&mut insts, BtInst::Match, budget)?;
    Ok(BtProgram { insts, group_count })
}

fn push_inst(
    insts: &mut Vec<BtInst>,
    inst: BtInst,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.check_instructions(insts.len().saturating_add(1))?;
    budget.charge_work(3)?;
    // Includes the cumulative allocation/copy envelope of geometric Vec growth.
    budget.charge_alloc(4 * std::mem::size_of::<BtInst>())?;
    insts.push(inst);
    Ok(())
}

fn emit(node: &Ast, insts: &mut Vec<BtInst>, budget: &mut Budget<'_>) -> Result<(), RegexError> {
    budget.charge_work(1)?;
    match node {
        Ast::Empty => {}
        Ast::Literal(c) => push_inst(insts, BtInst::Literal(*c), budget)?,
        Ast::Class(cl) => {
            budget.charge_alloc(2 * std::mem::size_of::<usize>())?;
            push_inst(
                insts,
                BtInst::Class(Rc::new(cl.budgeted_clone(budget)?)),
                budget,
            )?;
        }
        Ast::AnchorStart => push_inst(insts, BtInst::AssertStart, budget)?,
        Ast::AnchorEnd => push_inst(insts, BtInst::AssertEnd, budget)?,
        Ast::Backref(n) => push_inst(insts, BtInst::Backref(*n), budget)?,
        Ast::Concat(items) => {
            for item in items {
                emit(item, insts, budget)?;
            }
        }
        Ast::Alt(branches) => emit_alt(branches, insts, budget)?,
        Ast::Group { index, inner } => {
            if let Some(i) = index {
                push_inst(insts, BtInst::Save(2 * i), budget)?;
            }
            emit(inner, insts, budget)?;
            if let Some(i) = index {
                push_inst(insts, BtInst::Save(2 * i + 1), budget)?;
            }
        }
        Ast::Repeat {
            inner,
            min,
            max,
            greedy,
        } => emit_repeat(inner, *min, *max, *greedy, insts, budget)?,
    }
    Ok(())
}

fn emit_alt(
    branches: &[Ast],
    insts: &mut Vec<BtInst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let mut jmp_positions = Vec::new();
    let last = branches.len().saturating_sub(1);
    for (i, branch) in branches.iter().enumerate() {
        if i < last {
            let split_pos = insts.len();
            push_inst(insts, BtInst::Split(0, 0), budget)?;
            emit(branch, insts, budget)?;
            let jmp_pos = insts.len();
            push_inst(insts, BtInst::Jmp(0), budget)?;
            budget.charge_alloc(4 * std::mem::size_of::<usize>())?;
            jmp_positions.push(jmp_pos);
            let next = insts.len();
            insts[split_pos] = BtInst::Split(split_pos + 1, next);
        } else {
            emit(branch, insts, budget)?;
        }
    }
    let end = insts.len();
    for pos in jmp_positions {
        insts[pos] = BtInst::Jmp(end);
    }
    Ok(())
}

fn emit_repeat(
    inner: &Ast,
    min: u32,
    max: Option<u32>,
    greedy: bool,
    insts: &mut Vec<BtInst>,
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

/// Emits `inner*` with a loop guard so an empty-matching body cannot spin.
fn emit_star(
    inner: &Ast,
    greedy: bool,
    insts: &mut Vec<BtInst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let split_pos = insts.len();
    push_inst(insts, BtInst::Split(0, 0), budget)?;
    let mark_pos = insts.len();
    push_inst(insts, BtInst::MarkLoop, budget)?;
    emit(inner, insts, budget)?;
    let back_pos = insts.len();
    push_inst(insts, BtInst::LoopBack(split_pos, 0), budget)?;
    let after = insts.len();
    insts[split_pos] = if greedy {
        BtInst::Split(mark_pos, after)
    } else {
        BtInst::Split(after, mark_pos)
    };
    insts[back_pos] = BtInst::LoopBack(split_pos, after);
    Ok(())
}

/// Emits `count` nested optional copies of `inner`.
fn emit_optionals(
    inner: &Ast,
    count: usize,
    greedy: bool,
    insts: &mut Vec<BtInst>,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.charge_alloc_product(count, 4 * std::mem::size_of::<usize>())?;
    let mut splits = Vec::new();
    for _ in 0..count {
        budget.charge_work(1)?;
        let split_pos = insts.len();
        push_inst(insts, BtInst::Split(0, 0), budget)?;
        splits.push(split_pos);
        emit(inner, insts, budget)?;
    }
    let after = insts.len();
    for split_pos in splits {
        budget.charge_work(1)?;
        insts[split_pos] = if greedy {
            BtInst::Split(split_pos + 1, after)
        } else {
            BtInst::Split(after, split_pos + 1)
        };
    }
    Ok(())
}

/// A back-tracking thread.
#[derive(Clone)]
struct BtThread {
    pc: usize,
    sp: usize,
    caps: Captures,
    loops: Vec<usize>,
}

/// Attempts to match `program` anchored at position `start`.
///
/// `budget` is not a fresh allowance for this position: it is the number of
/// interpreter steps left in the whole search this attempt is one part of
/// (see [`find_from`]), and every step taken here is deducted from it in
/// place, so that the cost of trying many positions (or many matches; see
/// [`Regex::find_from`](crate::Regex::find_from)) adds up against one shared
/// total instead of each attempt getting its own fresh budget. Returns
/// `Ok(Some(caps))` on success, `Ok(None)` on failure, and
/// `Err(ResourceLimit { .. })` the moment the shared budget runs out.
#[allow(clippy::too_many_arguments)]
fn match_at(
    program: &BtProgram,
    chars: &[char],
    start: usize,
    full: bool,
    ci: bool,
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<Option<Captures>, RegexError> {
    budget.check_captures(program.group_count)?;
    let slots = program
        .group_count
        .checked_add(1)
        .and_then(|v| v.checked_mul(2))
        .ok_or(RegexError::ResourceLimit {
            stage: "capture slots",
            limit: budget.limits.captures,
        })?;
    budget.charge_work(slots)?;
    budget.charge_alloc_product(slots, std::mem::size_of::<Option<usize>>())?;
    budget.charge_alloc(4 * std::mem::size_of::<BtThread>())?;
    let len = chars.len();
    let mut stack: Vec<BtThread> = vec![BtThread {
        pc: 0,
        sp: start,
        caps: vec![None; slots],
        loops: Vec::new(),
    }];

    while let Some(mut t) = stack.pop() {
        loop {
            budget.charge_work(1)?;
            match &program.insts[t.pc] {
                BtInst::Literal(l) => {
                    if t.sp < len && char_matches(chars[t.sp], *l, ci, budget)? {
                        t.pc += 1;
                        t.sp += 1;
                    } else {
                        break;
                    }
                }
                BtInst::Class(cl) => {
                    if t.sp < len && cl.contains_bounded(chars[t.sp], ci, budget)? {
                        t.pc += 1;
                        t.sp += 1;
                    } else {
                        break;
                    }
                }
                BtInst::Match => {
                    if !full || t.sp == len {
                        return Ok(Some(t.caps));
                    }
                    break;
                }
                BtInst::Jmp(x) => t.pc = *x,
                BtInst::Split(a, b) => {
                    budget.charge_work(t.caps.len())?;
                    budget.charge_work(t.loops.len())?;
                    budget
                        .charge_alloc_product(t.caps.len(), std::mem::size_of::<Option<usize>>())?;
                    budget.charge_alloc_product(t.loops.len(), std::mem::size_of::<usize>())?;
                    budget.charge_alloc(4 * std::mem::size_of::<BtThread>())?;
                    let mut alt = t.clone();
                    alt.pc = *b;
                    stack.push(alt);
                    t.pc = *a;
                }
                BtInst::Save(slot) => {
                    if let Some(cell) = t.caps.get_mut(*slot) {
                        *cell = Some(t.sp);
                    }
                    t.pc += 1;
                }
                BtInst::Backref(n) => {
                    let group_start = t.caps.get(2 * n).copied().flatten();
                    let group_end = t.caps.get(2 * n + 1).copied().flatten();
                    match (group_start, group_end) {
                        (Some(gs), Some(ge)) => {
                            let width = ge - gs;
                            if width <= len - t.sp
                                && backref_matches(chars, t.sp, gs, ge, ci, budget)?
                            {
                                t.sp += width;
                                t.pc += 1;
                            } else {
                                break;
                            }
                        }
                        // A group that did not participate matches the empty
                        // string.
                        _ => t.pc += 1,
                    }
                }
                BtInst::AssertStart => {
                    if at_start(t.sp, chars, multiline) {
                        t.pc += 1;
                    } else {
                        break;
                    }
                }
                BtInst::AssertEnd => {
                    if at_end(t.sp, chars, multiline) {
                        t.pc += 1;
                    } else {
                        break;
                    }
                }
                BtInst::MarkLoop => {
                    budget.charge_alloc(4 * std::mem::size_of::<usize>())?;
                    t.loops.push(t.sp);
                    t.pc += 1;
                }
                BtInst::LoopBack(target, exit) => {
                    let entry = t.loops.pop().unwrap_or(t.sp);
                    if t.sp > entry {
                        t.pc = *target;
                    } else {
                        t.pc = *exit;
                    }
                }
            }
        }
    }
    Ok(None)
}

fn char_matches(
    input: char,
    literal: char,
    ci: bool,
    budget: &mut Budget<'_>,
) -> Result<bool, RegexError> {
    if ci {
        any_variant_bounded(input, |v| v == literal, budget)
    } else {
        budget.charge_work(1)?;
        Ok(input == literal)
    }
}

fn backref_matches(
    chars: &[char],
    sp: usize,
    gs: usize,
    ge: usize,
    ci: bool,
    budget: &mut Budget<'_>,
) -> Result<bool, RegexError> {
    let width = ge - gs;
    if width > chars.len() - sp {
        return Ok(false);
    }
    for offset in 0..width {
        if !char_matches(chars[sp + offset], chars[gs + offset], ci, budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Searches for the left-most match starting at or after `from`, returning the
/// capture slots (with slot 0/1 holding the match extent).
///
/// `budget` is shared across every `start` position this loop tries: it is
/// not reset between them, so the total interpreter work performed by this
/// one call is what is bounded, not the cost of any single position in
/// isolation. A caller that needs to run more than one such search as a
/// single logical operation can extend that sharing further by reusing the
/// same `budget` across those calls too, and [`Regex::replace`]
/// (crate::Regex::replace) / [`Regex::tokenize`](crate::Regex::tokenize) do
/// exactly that across every match found during one call, so a pattern whose
/// per-match cost is individually small still cannot buy unbounded total
/// work by matching over and over within that one `replace`/`tokenize` call.
/// The match iterator also keeps the same operation budget across its lifetime.
pub(crate) fn find_from(
    program: &BtProgram,
    chars: &[char],
    from: usize,
    ci: bool,
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<Option<Captures>, RegexError> {
    for start in from..=chars.len() {
        budget.charge_work(1)?;
        if let Some(caps) = match_at(program, chars, start, false, ci, multiline, budget)? {
            return Ok(Some(caps));
        }
    }
    Ok(None)
}

/// Returns whether any substring at or after position 0 matches (the semantics
/// of `fn:matches`).
pub(crate) fn is_match_search(
    program: &BtProgram,
    chars: &[char],
    ci: bool,
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<bool, RegexError> {
    Ok(find_from(program, chars, 0, ci, multiline, budget)?.is_some())
}

/// Returns whether the whole input matches when anchored at the start (the
/// XSD whole-string semantics). Not reached in normal use, since XSD patterns
/// never contain back-references, but kept for completeness.
pub(crate) fn is_match_full(
    program: &BtProgram,
    chars: &[char],
    ci: bool,
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<bool, RegexError> {
    Ok(match_at(program, chars, 0, true, ci, multiline, budget)?.is_some())
}
