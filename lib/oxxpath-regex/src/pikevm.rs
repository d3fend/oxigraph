//! A Pike VM that runs back-reference-free programs in linear time.
//!
//! The VM keeps a priority-ordered list of live threads, each carrying its own
//! capture slots. Threads that reach the same instruction at the same input
//! position are de-duplicated, so the work per position is bounded by the
//! program size; catastrophic back-tracking is therefore impossible. Capture
//! slots are shared through [`Rc`] and copied only when a `Save` instruction
//! writes one, keeping the common case cheap.

use std::rc::Rc;

use crate::budget::Budget;
use crate::error::RegexError;
use crate::program::{Inst, Program};

/// Captured slot positions: index `2*g` / `2*g+1` are the start/end of group
/// `g`, with group 0 being the whole match.
pub(crate) type Captures = Vec<Option<usize>>;

/// A live thread: a program counter and its capture slots.
#[derive(Clone)]
struct Thread {
    pc: usize,
    caps: Rc<Captures>,
}

/// Runs `program` over `chars`, starting the scan at `start`.
///
/// * `anchored` seeds a single thread at `start`; otherwise a fresh thread is
///   seeded at every position until a match is found, giving the left-most
///   match.
/// * `full` accepts only when the match reaches the end of the input, which
///   implements the whole-string anchoring of the XSD dialect.
///
/// Returns the winning capture slots, or `None` if there is no match.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    program: &Program,
    chars: &[char],
    start: usize,
    anchored: bool,
    full: bool,
    ci: bool,
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<Option<Captures>, RegexError> {
    let n = program.insts.len();
    budget.check_captures(program.group_count)?;
    let slots = program
        .group_count
        .checked_add(1)
        .and_then(|v| v.checked_mul(2))
        .ok_or(RegexError::ResourceLimit {
            stage: "capture slots",
            limit: budget.limits.captures,
        })?;
    budget.charge_work(n)?;
    budget.charge_work(slots)?;
    budget.charge_alloc_product(n, std::mem::size_of::<usize>())?;
    charge_captures(slots, budget)?;
    let len = chars.len();

    let mut clist: Vec<Thread> = Vec::new();
    let mut nlist: Vec<Thread> = Vec::new();
    let mut seen: Vec<usize> = vec![usize::MAX; n];
    let init: Rc<Captures> = Rc::new(vec![None; slots]);
    let mut matched: Option<Rc<Captures>> = None;

    add_thread(
        &mut clist,
        &mut seen,
        start + 1,
        program,
        0,
        start,
        Rc::clone(&init),
        chars,
        multiline,
        budget,
    )?;

    let mut sp = start;
    loop {
        let next_gen = sp + 2;
        let mut i = 0;
        while i < clist.len() {
            budget.charge_work(1)?;
            let thread = clist[i].clone();
            i += 1;
            match &program.insts[thread.pc] {
                Inst::Char(m) if sp < len && m.matches(chars[sp], ci, budget)? => {
                    add_thread(
                        &mut nlist,
                        &mut seen,
                        next_gen,
                        program,
                        thread.pc + 1,
                        sp + 1,
                        thread.caps,
                        chars,
                        multiline,
                        budget,
                    )?;
                }
                Inst::Match if !full || sp == len => {
                    matched = Some(thread.caps.clone());
                    // Lower-priority threads cannot produce a better match.
                    break;
                }
                _ => {}
            }
        }

        std::mem::swap(&mut clist, &mut nlist);
        nlist.clear();

        if !anchored && matched.is_none() && sp < len {
            add_thread(
                &mut clist,
                &mut seen,
                next_gen,
                program,
                0,
                sp + 1,
                Rc::clone(&init),
                chars,
                multiline,
                budget,
            )?;
        }

        if sp >= len {
            break;
        }
        sp += 1;
        if clist.is_empty() && (anchored || matched.is_some()) {
            break;
        }
    }

    match matched {
        None => Ok(None),
        Some(rc) => match Rc::try_unwrap(rc) {
            Ok(caps) => Ok(Some(caps)),
            Err(rc) => {
                charge_captures(rc.len(), budget)?;
                Ok(Some((*rc).clone()))
            }
        },
    }
}

/// Adds a thread and its epsilon closure to `list`, de-duplicating by program
/// counter within the generation `generation`. Uses an explicit work stack so
/// deep jump/split chains cannot overflow the native stack.
#[allow(clippy::too_many_arguments)]
fn add_thread(
    list: &mut Vec<Thread>,
    seen: &mut [usize],
    generation: usize,
    program: &Program,
    pc: usize,
    sp: usize,
    caps: Rc<Captures>,
    chars: &[char],
    multiline: bool,
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    let mut stack = Vec::new();
    push_stack(&mut stack, (pc, caps), budget)?;
    while let Some((pc, mut caps)) = stack.pop() {
        budget.charge_work(1)?;
        if seen[pc] == generation {
            continue;
        }
        seen[pc] = generation;
        match &program.insts[pc] {
            Inst::Jmp(t) => push_stack(&mut stack, (*t, caps), budget)?,
            Inst::Split(a, b) => {
                // Push the lower-priority branch first so the higher-priority
                // branch is expanded first (LIFO), preserving thread order.
                push_stack(&mut stack, (*b, Rc::clone(&caps)), budget)?;
                push_stack(&mut stack, (*a, caps), budget)?;
            }
            Inst::Save(slot) => {
                if Rc::strong_count(&caps) > 1 {
                    charge_captures(caps.len(), budget)?;
                }
                if let Some(cell) = Rc::make_mut(&mut caps).get_mut(*slot) {
                    *cell = Some(sp);
                }
                push_stack(&mut stack, (pc + 1, caps), budget)?;
            }
            Inst::AssertStart => {
                if at_start(sp, chars, multiline) {
                    push_stack(&mut stack, (pc + 1, caps), budget)?;
                }
            }
            Inst::AssertEnd => {
                if at_end(sp, chars, multiline) {
                    push_stack(&mut stack, (pc + 1, caps), budget)?;
                }
            }
            Inst::Char(_) | Inst::Match => {
                budget.charge_alloc(4 * std::mem::size_of::<Thread>())?;
                list.push(Thread { pc, caps });
            }
        }
    }
    Ok(())
}

fn charge_captures(slots: usize, budget: &mut Budget<'_>) -> Result<(), RegexError> {
    budget.charge_work(slots)?;
    budget.charge_alloc_product(slots, std::mem::size_of::<Option<usize>>())?;
    budget.charge_alloc(32)
}

fn push_stack(
    stack: &mut Vec<(usize, Rc<Captures>)>,
    item: (usize, Rc<Captures>),
    budget: &mut Budget<'_>,
) -> Result<(), RegexError> {
    budget.charge_work(1)?;
    budget.charge_alloc(4 * std::mem::size_of::<(usize, Rc<Captures>)>())?;
    stack.push(item);
    Ok(())
}

/// Whether position `sp` is at the start of the string, or of a line under
/// multi-line mode.
pub(crate) fn at_start(sp: usize, chars: &[char], multiline: bool) -> bool {
    // F&O 3.1 §5.6.1, flag `m`: `^` matches "the position immediately after a
    // newline character other than a newline character that is the last
    // character in the input string" — so a trailing newline opens no further
    // line, and `fn:matches("abcd&#xA;defg&#xA;", "^$", "m")` is false.
    sp == 0 || (multiline && sp > 0 && sp < chars.len() && chars[sp - 1] == '\n')
}

/// Whether position `sp` is at the end of the string, or of a line under
/// multi-line mode.
pub(crate) fn at_end(sp: usize, chars: &[char], multiline: bool) -> bool {
    (sp == chars.len() && (!multiline || chars.last() != Some(&'\n')))
        || (multiline && sp < chars.len() && chars[sp] == '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_trailing_newline_is_not_a_second_end_position() {
        assert!(at_end(0, &['\n'], true));
        assert!(!at_end(1, &['\n'], true));
        assert!(at_end(1, &['\n'], false));
        assert!(at_end(0, &[], true));
        assert!(at_end(1, &['a'], true));
    }
}
