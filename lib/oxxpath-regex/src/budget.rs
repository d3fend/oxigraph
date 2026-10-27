//! Resource limits shared by every phase of one regular-expression operation.

use crate::RegexError;

/// Deterministic implementation limits; exhaustion is never a pattern error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum UTF-8 pattern bytes admitted before parsing.
    pub pattern_bytes: usize,
    /// Maximum UTF-8 input bytes admitted before decoding.
    pub input_bytes: usize,
    /// Maximum UTF-8 output bytes.
    pub output_bytes: usize,
    /// Maximum charged primitive operations, including copies and comparisons.
    pub work: usize,
    /// Maximum cumulative requested allocation bytes during an operation.
    /// This conservative volume envelope also bounds simultaneously live storage.
    pub allocation_bytes: usize,
    /// Maximum recursive parser nesting, capped by the implementation at 200.
    pub nesting: usize,
    /// Maximum compiled instructions.
    pub instructions: usize,
    /// Maximum capturing groups, excluding group zero.
    pub captures: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            pattern_bytes: 1024 * 1024,
            input_bytes: 64 * 1024 * 1024,
            output_bytes: 64 * 1024 * 1024,
            work: 10_000_000,
            allocation_bytes: 64 * 1024 * 1024,
            nesting: 200,
            instructions: 200_000,
            captures: 4096,
        }
    }
}

/// One allowance, shared through all searches and output construction.
#[derive(Clone)]
pub(crate) struct Budget<'a> {
    pub(crate) limits: Limits,
    work: usize,
    allocated: usize,
    cancelled: Option<&'a dyn Fn() -> bool>,
}

impl std::fmt::Debug for Budget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("limits", &self.limits)
            .field("work", &self.work)
            .field("allocated", &self.allocated)
            .finish_non_exhaustive()
    }
}

impl<'a> Budget<'a> {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            work: 0,
            allocated: 0,
            cancelled: None,
        }
    }

    pub(crate) fn with_cancel(limits: Limits, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            cancelled: Some(cancelled),
            ..Self::new(limits)
        }
    }

    pub(crate) fn charge_work(&mut self, amount: usize) -> Result<(), RegexError> {
        if self.cancelled.is_some_and(|cancelled| cancelled()) {
            return Err(RegexError::Cancelled);
        }
        self.work = self
            .work
            .checked_add(amount)
            .ok_or(RegexError::ResourceLimit {
                stage: "work",
                limit: self.limits.work,
            })?;
        Self::check("work", self.work, self.limits.work)
    }

    pub(crate) fn charge_alloc(&mut self, amount: usize) -> Result<(), RegexError> {
        self.charge_work(0)?;
        self.allocated = self
            .allocated
            .checked_add(amount)
            .ok_or(RegexError::ResourceLimit {
                stage: "allocation bytes",
                limit: self.limits.allocation_bytes,
            })?;
        Self::check(
            "allocation bytes",
            self.allocated,
            self.limits.allocation_bytes,
        )
    }

    pub(crate) fn charge_alloc_product(
        &mut self,
        count: usize,
        width: usize,
    ) -> Result<(), RegexError> {
        let amount = count.checked_mul(width).ok_or(RegexError::ResourceLimit {
            stage: "allocation bytes",
            limit: self.limits.allocation_bytes,
        })?;
        self.charge_alloc(amount)
    }

    pub(crate) fn charge_work_product(
        &mut self,
        count: usize,
        width: usize,
    ) -> Result<(), RegexError> {
        let amount = count.checked_mul(width).ok_or(RegexError::ResourceLimit {
            stage: "work",
            limit: self.limits.work,
        })?;
        self.charge_work(amount)
    }

    pub(crate) fn check_pattern(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("pattern bytes", amount, self.limits.pattern_bytes)
    }
    pub(crate) fn check_input(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("input bytes", amount, self.limits.input_bytes)
    }
    pub(crate) fn check_output(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("output bytes", amount, self.limits.output_bytes)
    }
    pub(crate) fn check_nesting(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("nesting", amount, self.limits.nesting.min(200))
    }
    pub(crate) fn check_instructions(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("instructions", amount, self.limits.instructions)
    }
    pub(crate) fn check_captures(&self, amount: usize) -> Result<(), RegexError> {
        Self::check("captures", amount, self.limits.captures)
    }
    fn check(stage: &'static str, amount: usize, limit: usize) -> Result<(), RegexError> {
        if amount > limit {
            Err(RegexError::ResourceLimit { stage, limit })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_is_typed_and_checked_before_overflow() {
        let limits = Limits {
            work: 3,
            allocation_bytes: 8,
            ..Limits::default()
        };
        let mut budget = Budget::new(limits);
        assert_eq!(budget.charge_work(3), Ok(()));
        assert_eq!(
            budget.charge_work(1),
            Err(RegexError::ResourceLimit {
                stage: "work",
                limit: 3
            })
        );
        let mut budget = Budget::new(limits);
        assert_eq!(budget.charge_alloc(8), Ok(()));
        assert_eq!(
            budget.charge_alloc(1),
            Err(RegexError::ResourceLimit {
                stage: "allocation bytes",
                limit: 8
            })
        );
        let mut budget = Budget::new(Limits {
            allocation_bytes: usize::MAX,
            ..limits
        });
        assert!(matches!(
            budget.charge_alloc_product(usize::MAX, 2),
            Err(RegexError::ResourceLimit { .. })
        ));
    }

    #[test]
    fn cancellation_survives_zero_cost_and_allocation_checks() {
        let cancelled = || true;
        let mut budget = Budget::with_cancel(Limits::default(), &cancelled);
        assert_eq!(budget.charge_work(0), Err(RegexError::Cancelled));
        assert_eq!(budget.charge_alloc(0), Err(RegexError::Cancelled));
    }
}
