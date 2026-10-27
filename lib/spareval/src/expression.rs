#[cfg(feature = "sparql-12")]
use crate::ExpressionTriple;
use crate::dataset::ExpressionTerm;
use md5::{Digest, Md5};
use oxiri::Iri;
use oxixml_regex::{Limits as RegexLimits, Regex, RegexError, XsdVersion};
use oxrdf::vocab::{rdf, xsd};
#[cfg(feature = "sparql-12")]
use oxrdf::{BaseDirection, NamedOrBlankNode};
use oxrdf::{BlankNode, Literal, NamedNode, Term, Variable};
#[cfg(feature = "sep-0002")]
use oxsdatatypes::{Date, DayTimeDuration, Duration, Time, TimezoneOffset, YearMonthDuration};
use oxsdatatypes::{DateTime, Decimal, Double, Float, Integer};
use rand::random;
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};
use spargebra::algebra::Function;
use sparopt::algebra::{Expression, GraphPattern};
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;

pub type CustomFunctionRegistry =
    HashMap<NamedNode, Arc<dyn (Fn(&[Term]) -> Option<Term>) + Send + Sync>>;

pub trait ExpressionEvaluatorContext<'a> {
    type Term: Clone + Eq + 'a;
    type Tuple: 'a;
    type Error: 'a;

    fn build_variable_lookup(
        &mut self,
        variable: &Variable,
    ) -> impl Fn(&Self::Tuple) -> Option<Self::Term> + 'a;
    fn build_is_variable_bound(
        &mut self,
        variable: &Variable,
    ) -> impl Fn(&Self::Tuple) -> bool + 'a;
    fn build_exists(
        &mut self,
        plan: &GraphPattern,
    ) -> Result<impl Fn(&Self::Tuple) -> Result<bool, Self::Error> + 'a, Self::Error>;
    fn internalize_named_node(&mut self, term: &NamedNode) -> Result<Self::Term, Self::Error>;
    fn internalize_literal(&mut self, term: &Literal) -> Result<Self::Term, Self::Error>;
    fn build_internalize_expression_term(
        &mut self,
    ) -> impl Fn(ExpressionTerm) -> Option<Self::Term> + 'a; // TODO: return result
    fn build_externalize_expression_term(
        &mut self,
    ) -> impl Fn(Self::Term) -> Option<ExpressionTerm> + 'a; // TODO: return result
    fn now(&mut self) -> DateTime;
    fn base_iri(&mut self) -> Option<Arc<Iri<String>>>;
    fn custom_functions(&mut self) -> &CustomFunctionRegistry;
    fn cancellation_token(&self) -> Option<crate::CancellationToken> {
        None
    }
}

pub type ExpressionEvaluator<'a, I, O, E> =
    Rc<dyn Fn(&I) -> Result<Option<O>, ExpressionEvaluationError<E>> + 'a>;

// Ordinary SPARQL expression errors remain None; evaluated fatal errors use ?.
macro_rules! some_or_none {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}

#[derive(Debug, Error)]
pub enum ExpressionEvaluationError<C> {
    /// A configured execution resource limit was exceeded.
    #[error("The SPARQL operation exceeded the {stage} resource limit ({limit})")]
    ResourceLimit { stage: &'static str, limit: usize },
    /// An unexpected regular expression engine failure.
    #[error("Regular expression evaluation failed: {0}")]
    Regex(#[source] RegexError),
    /// The caller cancelled an executing expression.
    #[error("The SPARQL operation was cancelled")]
    Cancelled,
    /// Error from the evaluation context
    #[error(transparent)]
    Context(C),
    /// The given custom function is not supported
    #[error("The custom function {0} is not supported")]
    UnsupportedCustomFunction(NamedNode),
    /// The given custom function arity is not supported
    #[error("The custom function {name} requires between {} and {} arguments, but {actual} were given", .expected.start(), .expected.end())]
    UnsupportedCustomFunctionArity {
        name: NamedNode,
        expected: RangeInclusive<usize>,
        actual: usize,
    },
}

impl<C> From<RegexError> for ExpressionEvaluationError<C> {
    fn from(error: RegexError) -> Self {
        match error {
            RegexError::ResourceLimit { stage, limit } => Self::ResourceLimit { stage, limit },
            RegexError::Cancelled => Self::Cancelled,
            other => Self::Regex(other),
        }
    }
}

pub fn build_expression_evaluator<'a, C: ExpressionEvaluatorContext<'a>>(
    expression: &Expression,
    context: &mut C,
) -> Result<
    ExpressionEvaluator<'a, C::Tuple, ExpressionTerm, C::Error>,
    ExpressionEvaluationError<C::Error>,
> {
    Ok(match expression {
        Expression::NamedNode(t) => {
            let t = ExpressionTerm::from(Term::from(t.clone()));
            Rc::new(move |_| Ok(Some(t.clone())))
        }
        Expression::Literal(t) => {
            let t = ExpressionTerm::from(Term::from(t.clone()));
            Rc::new(move |_| Ok(Some(t.clone())))
        }
        Expression::Variable(v) => {
            let lookup = context.build_variable_lookup(v);
            let externalize = context.build_externalize_expression_term();
            Rc::new(move |t| Ok(externalize(some_or_none!(lookup(t)))))
        }
        Expression::Bound(v) => {
            let lookup = context.build_is_variable_bound(v);
            Rc::new(move |tuple| Ok(Some(lookup(tuple).into())))
        }
        Expression::Exists(plan) => {
            let exists = context
                .build_exists(plan)
                .map_err(ExpressionEvaluationError::Context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    exists(tuple)
                        .map_err(ExpressionEvaluationError::Context)?
                        .into(),
                ))
            })
        }
        Expression::Or(children) => {
            let children = children
                .iter()
                .map(|i| build_expression_evaluator(i, context))
                .collect::<Result<Vec<_>, _>>()?;
            Rc::new(move |tuple| {
                let mut error = false;
                for child in &children {
                    match child(tuple)?.and_then(|e| e.effective_boolean_value()) {
                        Some(true) => return Ok(Some(true.into())),
                        Some(false) => (),
                        None => error = true,
                    }
                }
                Ok(if error { None } else { Some(false.into()) })
            })
        }
        Expression::And(children) => {
            let children = children
                .iter()
                .map(|i| build_expression_evaluator(i, context))
                .collect::<Result<Vec<_>, _>>()?;
            Rc::new(move |tuple| {
                let mut error = false;
                for child in &children {
                    match child(tuple)?.and_then(|e| e.effective_boolean_value()) {
                        Some(true) => (),
                        Some(false) => return Ok(Some(false.into())),
                        None => error = true,
                    }
                }
                Ok(if error { None } else { Some(true.into()) })
            })
        }
        Expression::Equal(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(equals(&some_or_none!(a(tuple)?), &some_or_none!(b(tuple)?)).map(Into::into))
            })
        }
        Expression::SameTerm(a, b) => {
            match (
                try_build_internal_expression_evaluator(a, context)?,
                try_build_internal_expression_evaluator(b, context)?,
            ) {
                (Some(a), Some(b)) => Rc::new(move |tuple| {
                    Ok(Some(
                        (some_or_none!(a(tuple)?) == some_or_none!(b(tuple)?)).into(),
                    ))
                }),
                (Some(a), None) => {
                    let b = build_expression_evaluator(b, context)?;
                    let internalize = context.build_internalize_expression_term();
                    Rc::new(move |tuple| {
                        Ok(Some(
                            (some_or_none!(a(tuple)?)
                                == some_or_none!(internalize(some_or_none!(b(tuple)?))))
                            .into(),
                        ))
                    })
                }
                (None, Some(b)) => {
                    let a = build_expression_evaluator(a, context)?;
                    let internalize = context.build_internalize_expression_term();
                    Rc::new(move |tuple| {
                        Ok(Some(
                            (some_or_none!(internalize(some_or_none!(a(tuple)?)))
                                == some_or_none!(b(tuple)?))
                            .into(),
                        ))
                    })
                }
                (None, None) => {
                    let a = build_expression_evaluator(a, context)?;
                    let b = build_expression_evaluator(b, context)?;
                    Rc::new(move |tuple| {
                        Ok(Some(
                            (some_or_none!(a(tuple)?) == some_or_none!(b(tuple)?)).into(),
                        ))
                    })
                }
            }
        }
        Expression::Greater(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    (some_or_none!(partial_cmp(
                        &some_or_none!(a(tuple)?),
                        &some_or_none!(b(tuple)?)
                    )) == Ordering::Greater)
                        .into(),
                ))
            })
        }
        Expression::GreaterOrEqual(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(partial_cmp(
                        &some_or_none!(a(tuple)?),
                        &some_or_none!(b(tuple)?)
                    )) {
                        Ordering::Greater | Ordering::Equal => true,
                        Ordering::Less => false,
                    }
                    .into(),
                ))
            })
        }
        Expression::Less(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    (some_or_none!(partial_cmp(
                        &some_or_none!(a(tuple)?),
                        &some_or_none!(b(tuple)?)
                    )) == Ordering::Less)
                        .into(),
                ))
            })
        }
        Expression::LessOrEqual(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(partial_cmp(
                        &some_or_none!(a(tuple)?),
                        &some_or_none!(b(tuple)?)
                    )) {
                        Ordering::Less | Ordering::Equal => true,
                        Ordering::Greater => false,
                    }
                    .into(),
                ))
            })
        }
        Expression::Add(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(NumericBinaryOperands::new(
                        some_or_none!(a(tuple)?),
                        some_or_none!(b(tuple)?)
                    )) {
                        NumericBinaryOperands::Float(v1, v2) => {
                            ExpressionTerm::FloatLiteral(v1 + v2)
                        }
                        NumericBinaryOperands::Double(v1, v2) => {
                            ExpressionTerm::DoubleLiteral(v1 + v2)
                        }
                        NumericBinaryOperands::Integer(v1, v2) => {
                            ExpressionTerm::IntegerLiteral(some_or_none!(v1.checked_add(v2)))
                        }
                        NumericBinaryOperands::Decimal(v1, v2) => {
                            ExpressionTerm::DecimalLiteral(some_or_none!(v1.checked_add(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::Duration(v1, v2) => {
                            ExpressionTerm::DurationLiteral(some_or_none!(v1.checked_add(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::YearMonthDuration(v1, v2) => {
                            ExpressionTerm::YearMonthDurationLiteral(some_or_none!(
                                v1.checked_add(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DayTimeDuration(v1, v2) => {
                            ExpressionTerm::DayTimeDurationLiteral(some_or_none!(
                                v1.checked_add(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_add_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeYearMonthDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_add_year_month_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeDayTimeDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_add_day_time_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(v1.checked_add_duration(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateYearMonthDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(
                                v1.checked_add_year_month_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateDayTimeDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(
                                v1.checked_add_day_time_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::TimeDuration(v1, v2) => {
                            ExpressionTerm::TimeLiteral(some_or_none!(v1.checked_add_duration(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::TimeDayTimeDuration(v1, v2) => {
                            ExpressionTerm::TimeLiteral(some_or_none!(
                                v1.checked_add_day_time_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTime(_, _)
                        | NumericBinaryOperands::Time(_, _)
                        | NumericBinaryOperands::Date(_, _) => return Ok(None),
                    },
                ))
            })
        }
        Expression::Subtract(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(NumericBinaryOperands::new(
                        some_or_none!(a(tuple)?),
                        some_or_none!(b(tuple)?)
                    )) {
                        NumericBinaryOperands::Float(v1, v2) => {
                            ExpressionTerm::FloatLiteral(v1 - v2)
                        }
                        NumericBinaryOperands::Double(v1, v2) => {
                            ExpressionTerm::DoubleLiteral(v1 - v2)
                        }
                        NumericBinaryOperands::Integer(v1, v2) => {
                            ExpressionTerm::IntegerLiteral(some_or_none!(v1.checked_sub(v2)))
                        }
                        NumericBinaryOperands::Decimal(v1, v2) => {
                            ExpressionTerm::DecimalLiteral(some_or_none!(v1.checked_sub(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTime(v1, v2) => {
                            ExpressionTerm::DayTimeDurationLiteral(some_or_none!(
                                v1.checked_sub(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::Date(v1, v2) => {
                            ExpressionTerm::DayTimeDurationLiteral(some_or_none!(
                                v1.checked_sub(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::Time(v1, v2) => {
                            ExpressionTerm::DayTimeDurationLiteral(some_or_none!(
                                v1.checked_sub(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::Duration(v1, v2) => {
                            ExpressionTerm::DurationLiteral(some_or_none!(v1.checked_sub(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::YearMonthDuration(v1, v2) => {
                            ExpressionTerm::YearMonthDurationLiteral(some_or_none!(
                                v1.checked_sub(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DayTimeDuration(v1, v2) => {
                            ExpressionTerm::DayTimeDurationLiteral(some_or_none!(
                                v1.checked_sub(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_sub_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeYearMonthDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_sub_year_month_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateTimeDayTimeDuration(v1, v2) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                v1.checked_sub_day_time_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(v1.checked_sub_duration(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateYearMonthDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(
                                v1.checked_sub_year_month_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::DateDayTimeDuration(v1, v2) => {
                            ExpressionTerm::DateLiteral(some_or_none!(
                                v1.checked_sub_day_time_duration(v2)
                            ))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::TimeDuration(v1, v2) => {
                            ExpressionTerm::TimeLiteral(some_or_none!(v1.checked_sub_duration(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        NumericBinaryOperands::TimeDayTimeDuration(v1, v2) => {
                            ExpressionTerm::TimeLiteral(some_or_none!(
                                v1.checked_sub_day_time_duration(v2)
                            ))
                        }
                    },
                ))
            })
        }
        Expression::Multiply(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(NumericBinaryOperands::new(
                        some_or_none!(a(tuple)?),
                        some_or_none!(b(tuple)?)
                    )) {
                        NumericBinaryOperands::Float(v1, v2) => {
                            ExpressionTerm::FloatLiteral(v1 * v2)
                        }
                        NumericBinaryOperands::Double(v1, v2) => {
                            ExpressionTerm::DoubleLiteral(v1 * v2)
                        }
                        NumericBinaryOperands::Integer(v1, v2) => {
                            ExpressionTerm::IntegerLiteral(some_or_none!(v1.checked_mul(v2)))
                        }
                        NumericBinaryOperands::Decimal(v1, v2) => {
                            ExpressionTerm::DecimalLiteral(some_or_none!(v1.checked_mul(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        _ => return Ok(None),
                    },
                ))
            })
        }
        Expression::Divide(a, b) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    match some_or_none!(NumericBinaryOperands::new(
                        some_or_none!(a(tuple)?),
                        some_or_none!(b(tuple)?)
                    )) {
                        NumericBinaryOperands::Float(v1, v2) => {
                            ExpressionTerm::FloatLiteral(v1 / v2)
                        }
                        NumericBinaryOperands::Double(v1, v2) => {
                            ExpressionTerm::DoubleLiteral(v1 / v2)
                        }
                        NumericBinaryOperands::Integer(v1, v2) => ExpressionTerm::DecimalLiteral(
                            some_or_none!(Decimal::from(v1).checked_div(v2)),
                        ),
                        NumericBinaryOperands::Decimal(v1, v2) => {
                            ExpressionTerm::DecimalLiteral(some_or_none!(v1.checked_div(v2)))
                        }
                        #[cfg(feature = "sep-0002")]
                        _ => return Ok(None),
                    },
                ))
            })
        }
        Expression::UnaryPlus(e) => {
            let e = build_expression_evaluator(e, context)?;
            Rc::new(move |tuple| {
                Ok(Some(match some_or_none!(e(tuple)?) {
                    ExpressionTerm::FloatLiteral(value) => ExpressionTerm::FloatLiteral(value),
                    ExpressionTerm::DoubleLiteral(value) => ExpressionTerm::DoubleLiteral(value),
                    ExpressionTerm::IntegerLiteral(value) => ExpressionTerm::IntegerLiteral(value),
                    ExpressionTerm::DecimalLiteral(value) => ExpressionTerm::DecimalLiteral(value),
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::DurationLiteral(value) => {
                        ExpressionTerm::DurationLiteral(value)
                    }
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::YearMonthDurationLiteral(value) => {
                        ExpressionTerm::YearMonthDurationLiteral(value)
                    }
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::DayTimeDurationLiteral(value) => {
                        ExpressionTerm::DayTimeDurationLiteral(value)
                    }
                    _ => return Ok(None),
                }))
            })
        }
        Expression::UnaryMinus(e) => {
            let e = build_expression_evaluator(e, context)?;
            Rc::new(move |tuple| {
                Ok(Some(match some_or_none!(e(tuple)?) {
                    ExpressionTerm::FloatLiteral(value) => ExpressionTerm::FloatLiteral(-value),
                    ExpressionTerm::DoubleLiteral(value) => ExpressionTerm::DoubleLiteral(-value),
                    ExpressionTerm::IntegerLiteral(value) => {
                        ExpressionTerm::IntegerLiteral(some_or_none!(value.checked_neg()))
                    }
                    ExpressionTerm::DecimalLiteral(value) => {
                        ExpressionTerm::DecimalLiteral(some_or_none!(value.checked_neg()))
                    }
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::DurationLiteral(value) => {
                        ExpressionTerm::DurationLiteral(some_or_none!(value.checked_neg()))
                    }
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::YearMonthDurationLiteral(value) => {
                        ExpressionTerm::YearMonthDurationLiteral(some_or_none!(value.checked_neg()))
                    }
                    #[cfg(feature = "sep-0002")]
                    ExpressionTerm::DayTimeDurationLiteral(value) => {
                        ExpressionTerm::DayTimeDurationLiteral(some_or_none!(value.checked_neg()))
                    }
                    _ => return Ok(None),
                }))
            })
        }
        Expression::Not(e) => {
            let e = build_expression_evaluator(e, context)?;
            Rc::new(move |tuple| {
                Ok(Some(
                    (!some_or_none!(some_or_none!(e(tuple)?).effective_boolean_value())).into(),
                ))
            })
        }
        Expression::Coalesce(l) => {
            let l = l
                .iter()
                .map(|e| build_expression_evaluator(e, context))
                .collect::<Result<Vec<_>, _>>()?;
            Rc::new(move |tuple| {
                for e in &l {
                    if let Some(result) = e(tuple)? {
                        return Ok(Some(result));
                    }
                }
                Ok(None)
            })
        }
        Expression::If(a, b, c) => {
            let a = build_expression_evaluator(a, context)?;
            let b = build_expression_evaluator(b, context)?;
            let c = build_expression_evaluator(c, context)?;
            Rc::new(move |tuple| {
                Ok(
                    if some_or_none!(some_or_none!(a(tuple)?).effective_boolean_value()) {
                        b(tuple)?
                    } else {
                        c(tuple)?
                    },
                )
            })
        }
        Expression::FunctionCall(function, parameters) => match function {
            Function::Str => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::StringLiteral(
                        match some_or_none!(e(tuple)?).into() {
                            Term::NamedNode(term) => term.into_string(),
                            Term::BlankNode(_) => return Ok(None),
                            Term::Literal(term) => term.destruct().0,
                            #[cfg(feature = "sparql-12")]
                            Term::Triple(_) => return Ok(None),
                        },
                    )))
                })
            }
            Function::Lang => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::StringLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::LangStringLiteral { language, .. } => language,
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::DirLangStringLiteral { language, .. } => language,
                            ExpressionTerm::NamedNode(_) | ExpressionTerm::BlankNode(_) => {
                                return Ok(None);
                            }
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::Triple(_) => return Ok(None),
                            _ => String::new(),
                        },
                    )))
                })
            }
            Function::LangMatches => {
                let language_tag = build_expression_evaluator(&parameters[0], context)?;
                let language_range = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let ExpressionTerm::StringLiteral(mut language_tag) =
                        some_or_none!(language_tag(tuple)?)
                    else {
                        return Ok(None);
                    };
                    language_tag.make_ascii_lowercase();
                    let ExpressionTerm::StringLiteral(mut language_range) =
                        some_or_none!(language_range(tuple)?)
                    else {
                        return Ok(None);
                    };
                    language_range.make_ascii_lowercase();
                    Ok(Some(
                        if &*language_range == "*" {
                            !language_tag.is_empty()
                        } else {
                            !ZipLongest::new(language_range.split('-'), language_tag.split('-'))
                                .any(|parts| match parts {
                                    (Some(range_subtag), Some(language_subtag)) => {
                                        range_subtag != language_subtag
                                    }
                                    (Some(_), None) => true,
                                    (None, _) => false,
                                })
                        }
                        .into(),
                    ))
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::LangDir => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::StringLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DirLangStringLiteral { direction, .. } => {
                                match direction {
                                    BaseDirection::Ltr => "ltr".into(),
                                    BaseDirection::Rtl => "rtl".into(),
                                }
                            }
                            ExpressionTerm::NamedNode(_) | ExpressionTerm::BlankNode(_) => {
                                return Ok(None);
                            }
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::Triple(_) => return Ok(None),
                            _ => String::new(),
                        },
                    )))
                })
            }
            Function::Datatype => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::NamedNode(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::StringLiteral(_) => xsd::STRING.into(),
                            ExpressionTerm::LangStringLiteral { .. } => rdf::LANG_STRING.into(),
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::DirLangStringLiteral { .. } => {
                                rdf::DIR_LANG_STRING.into()
                            }
                            ExpressionTerm::BooleanLiteral(_) => xsd::BOOLEAN.into(),
                            ExpressionTerm::IntegerLiteral(_) => xsd::INTEGER.into(),
                            ExpressionTerm::DecimalLiteral(_) => xsd::DECIMAL.into(),
                            ExpressionTerm::FloatLiteral(_) => xsd::FLOAT.into(),
                            ExpressionTerm::DoubleLiteral(_) => xsd::DOUBLE.into(),
                            ExpressionTerm::DateTimeLiteral(_) => xsd::DATE_TIME.into(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DateLiteral(_) => xsd::DATE.into(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::TimeLiteral(_) => xsd::TIME.into(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearLiteral(_) => xsd::G_YEAR.into(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearMonthLiteral(_) => xsd::G_YEAR_MONTH.into(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthLiteral(_) => xsd::G_MONTH.into(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthDayLiteral(_) => xsd::G_MONTH_DAY.into(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GDayLiteral(_) => xsd::G_DAY.into(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DurationLiteral(_) => xsd::DURATION.into(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::YearMonthDurationLiteral(_) => {
                                xsd::YEAR_MONTH_DURATION.into()
                            }
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DayTimeDurationLiteral(_) => {
                                xsd::DAY_TIME_DURATION.into()
                            }
                            ExpressionTerm::OtherTypedLiteral { datatype, .. } => datatype,
                            ExpressionTerm::NamedNode(_) | ExpressionTerm::BlankNode(_) => {
                                return Ok(None);
                            }
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::Triple(_) => return Ok(None),
                        },
                    )))
                })
            }
            Function::Iri => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                let base_iri = context.base_iri();
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::NamedNode(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::NamedNode(iri) => iri,
                            ExpressionTerm::StringLiteral(iri) => {
                                let iri = if let Some(base_iri) = &base_iri {
                                    base_iri.resolve(&iri)
                                } else {
                                    Iri::parse(iri)
                                };
                                some_or_none!(iri.ok()).into()
                            }
                            _ => return Ok(None),
                        },
                    )))
                })
            }
            Function::BNode => match parameters.first() {
                Some(id) => {
                    let id = build_expression_evaluator(id, context)?;
                    Rc::new(move |tuple| {
                        let ExpressionTerm::StringLiteral(id) = some_or_none!(id(tuple)?) else {
                            return Ok(None);
                        };
                        Ok(Some(ExpressionTerm::BlankNode(some_or_none!(
                            BlankNode::new(id).ok()
                        ))))
                    })
                }
                None => Rc::new(|_| Ok(Some(ExpressionTerm::BlankNode(BlankNode::default())))),
            },
            Function::Rand => {
                Rc::new(|_| Ok(Some(ExpressionTerm::DoubleLiteral(random::<f64>().into()))))
            }
            Function::Abs => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(match some_or_none!(e(tuple)?) {
                        ExpressionTerm::IntegerLiteral(value) => Some(
                            ExpressionTerm::IntegerLiteral(some_or_none!(value.checked_abs())),
                        ),
                        ExpressionTerm::DecimalLiteral(value) => Some(
                            ExpressionTerm::DecimalLiteral(some_or_none!(value.checked_abs())),
                        ),
                        ExpressionTerm::FloatLiteral(value) => {
                            Some(ExpressionTerm::FloatLiteral(value.abs()))
                        }
                        ExpressionTerm::DoubleLiteral(value) => {
                            Some(ExpressionTerm::DoubleLiteral(value.abs()))
                        }
                        _ => None,
                    })
                })
            }
            Function::Ceil => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(match some_or_none!(e(tuple)?) {
                        ExpressionTerm::IntegerLiteral(value) => {
                            Some(ExpressionTerm::IntegerLiteral(value))
                        }
                        ExpressionTerm::DecimalLiteral(value) => Some(
                            ExpressionTerm::DecimalLiteral(some_or_none!(value.checked_ceil())),
                        ),
                        ExpressionTerm::FloatLiteral(value) => {
                            Some(ExpressionTerm::FloatLiteral(value.ceil()))
                        }
                        ExpressionTerm::DoubleLiteral(value) => {
                            Some(ExpressionTerm::DoubleLiteral(value.ceil()))
                        }
                        _ => None,
                    })
                })
            }
            Function::Floor => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(match some_or_none!(e(tuple)?) {
                        ExpressionTerm::IntegerLiteral(value) => {
                            Some(ExpressionTerm::IntegerLiteral(value))
                        }
                        ExpressionTerm::DecimalLiteral(value) => Some(
                            ExpressionTerm::DecimalLiteral(some_or_none!(value.checked_floor())),
                        ),
                        ExpressionTerm::FloatLiteral(value) => {
                            Some(ExpressionTerm::FloatLiteral(value.floor()))
                        }
                        ExpressionTerm::DoubleLiteral(value) => {
                            Some(ExpressionTerm::DoubleLiteral(value.floor()))
                        }
                        _ => None,
                    })
                })
            }
            Function::Round => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(match some_or_none!(e(tuple)?) {
                        ExpressionTerm::IntegerLiteral(value) => {
                            Some(ExpressionTerm::IntegerLiteral(value))
                        }
                        ExpressionTerm::DecimalLiteral(value) => Some(
                            ExpressionTerm::DecimalLiteral(some_or_none!(value.checked_round())),
                        ),
                        ExpressionTerm::FloatLiteral(value) => {
                            Some(ExpressionTerm::FloatLiteral(value.round()))
                        }
                        ExpressionTerm::DoubleLiteral(value) => {
                            Some(ExpressionTerm::DoubleLiteral(value.round()))
                        }
                        _ => None,
                    })
                })
            }
            Function::Concat => {
                let l = parameters
                    .iter()
                    .map(|e| build_expression_evaluator(e, context))
                    .collect::<Result<Vec<_>, _>>()?;
                Rc::new(move |tuple| {
                    let mut result = String::default();
                    let mut language = None;
                    for e in &l {
                        let (value, e_language) =
                            some_or_none!(to_string_and_language(some_or_none!(e(tuple)?)));
                        if let Some(lang) = &language {
                            if *lang != e_language {
                                language = Some(None)
                            }
                        } else {
                            language = Some(e_language)
                        }
                        result += &value
                    }
                    Ok(Some(build_plain_literal(result, language.flatten())))
                })
            }
            Function::SubStr => {
                let source = build_expression_evaluator(&parameters[0], context)?;
                let starting_loc = build_expression_evaluator(&parameters[1], context)?;
                let length = parameters
                    .get(2)
                    .map(|l| build_expression_evaluator(l, context))
                    .transpose()?;
                Rc::new(move |tuple| {
                    let (source, language) =
                        some_or_none!(to_string_and_language(some_or_none!(source(tuple)?)));

                    let starting_location: usize = if let ExpressionTerm::IntegerLiteral(v) =
                        some_or_none!(starting_loc(tuple)?)
                    {
                        some_or_none!(usize::try_from(i64::from(v)).ok())
                    } else {
                        return Ok(None);
                    };
                    let length = if let Some(length) = &length {
                        if let ExpressionTerm::IntegerLiteral(v) = some_or_none!(length(tuple)?) {
                            Some(some_or_none!(usize::try_from(i64::from(v)).ok()))
                        } else {
                            return Ok(None);
                        }
                    } else {
                        None
                    };

                    // We want to slice on char indices, not byte indices
                    let mut start_iter = source
                        .char_indices()
                        .skip(some_or_none!(starting_location.checked_sub(1)))
                        .peekable();
                    let result = if let Some((start_position, _)) = start_iter.peek().copied() {
                        if let Some(length) = length {
                            let mut end_iter = start_iter.skip(length).peekable();
                            if let Some((end_position, _)) = end_iter.peek() {
                                &source[start_position..*end_position]
                            } else {
                                &source[start_position..]
                            }
                        } else {
                            &source[start_position..]
                        }
                    } else {
                        ""
                    };
                    Ok(Some(build_plain_literal(result.into(), language)))
                })
            }
            Function::StrLen => {
                let arg = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    let (string, _) =
                        some_or_none!(to_string_and_language(some_or_none!(arg(tuple)?)));
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        some_or_none!(i64::try_from(string.chars().count()).ok()).into(),
                    )))
                })
            }
            Function::Replace => {
                let cancellation = context.cancellation_token().unwrap_or_default();
                let arg = build_expression_evaluator(&parameters[0], context)?;
                let replacement = build_expression_evaluator(&parameters[2], context)?;
                if let Some(regex) = static_pattern_if_exists(&parameters[1], parameters.get(3)) {
                    Rc::new(move |tuple| {
                        let regex =
                            some_or_none!(regex.get_with_cancel(&|| cancellation.is_cancelled())?);
                        let (text, language) =
                            some_or_none!(to_string_and_language(some_or_none!(arg(tuple)?)));
                        let ExpressionTerm::StringLiteral(replacement) =
                            some_or_none!(replacement(tuple)?)
                        else {
                            return Ok(None);
                        };
                        Ok(Some(build_plain_literal(
                            some_or_none!(replace_xpath_with_cancel(
                                regex,
                                &text,
                                &replacement,
                                &|| cancellation.is_cancelled(),
                            )?),
                            language,
                        )))
                    })
                } else {
                    let pattern = build_expression_evaluator(&parameters[1], context)?;
                    let flags = parameters
                        .get(3)
                        .map(|flags| build_expression_evaluator(flags, context))
                        .transpose()?;
                    Rc::new(move |tuple| {
                        let ExpressionTerm::StringLiteral(pattern) = some_or_none!(pattern(tuple)?)
                        else {
                            return Ok(None);
                        };
                        let options = if let Some(flags) = &flags {
                            let ExpressionTerm::StringLiteral(options) =
                                some_or_none!(flags(tuple)?)
                            else {
                                return Ok(None);
                            };
                            Some(options)
                        } else {
                            None
                        };
                        let regex = some_or_none!(compile_pattern_with_cancel(
                            &pattern,
                            options.as_deref(),
                            &|| cancellation.is_cancelled()
                        )?);
                        let (text, language) =
                            some_or_none!(to_string_and_language(some_or_none!(arg(tuple)?)));
                        let ExpressionTerm::StringLiteral(replacement) =
                            some_or_none!(replacement(tuple)?)
                        else {
                            return Ok(None);
                        };
                        Ok(Some(build_plain_literal(
                            some_or_none!(replace_xpath_with_cancel(
                                &regex,
                                &text,
                                &replacement,
                                &|| cancellation.is_cancelled(),
                            )?),
                            language,
                        )))
                    })
                }
            }
            Function::UCase => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    let (value, language) =
                        some_or_none!(to_string_and_language(some_or_none!(e(tuple)?)));
                    Ok(Some(build_plain_literal(value.to_uppercase(), language)))
                })
            }
            Function::LCase => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    let (value, language) =
                        some_or_none!(to_string_and_language(some_or_none!(e(tuple)?)));
                    Ok(Some(build_plain_literal(value.to_lowercase(), language)))
                })
            }
            Function::StrStarts => {
                let arg1 = build_expression_evaluator(&parameters[0], context)?;
                let arg2 = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let (arg1, arg2, _) = some_or_none!(to_argument_compatible_strings(
                        some_or_none!(arg1(tuple)?),
                        some_or_none!(arg2(tuple)?)
                    ));
                    Ok(Some(arg1.starts_with(arg2.as_str()).into()))
                })
            }
            Function::EncodeForUri => {
                let ltrl = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    let (ltlr, _) =
                        some_or_none!(to_string_and_language(some_or_none!(ltrl(tuple)?)));
                    let mut result = Vec::with_capacity(ltlr.len());
                    for c in ltlr.bytes() {
                        match c {
                            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                                result.push(c)
                            }
                            _ => {
                                result.push(b'%');
                                let high = c / 16;
                                let low = c % 16;
                                result.push(if high < 10 {
                                    b'0' + high
                                } else {
                                    b'A' + (high - 10)
                                });
                                result.push(if low < 10 {
                                    b'0' + low
                                } else {
                                    b'A' + (low - 10)
                                });
                            }
                        }
                    }
                    Ok(Some(ExpressionTerm::StringLiteral(some_or_none!(
                        String::from_utf8(result).ok()
                    ))))
                })
            }
            Function::StrEnds => {
                let arg1 = build_expression_evaluator(&parameters[0], context)?;
                let arg2 = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let (arg1, arg2, _) = some_or_none!(to_argument_compatible_strings(
                        some_or_none!(arg1(tuple)?),
                        some_or_none!(arg2(tuple)?)
                    ));
                    Ok(Some(arg1.ends_with(arg2.as_str()).into()))
                })
            }
            Function::Contains => {
                let arg1 = build_expression_evaluator(&parameters[0], context)?;
                let arg2 = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let (arg1, arg2, _) = some_or_none!(to_argument_compatible_strings(
                        some_or_none!(arg1(tuple)?),
                        some_or_none!(arg2(tuple)?)
                    ));
                    Ok(Some(arg1.contains(arg2.as_str()).into()))
                })
            }
            Function::StrBefore => {
                let arg1 = build_expression_evaluator(&parameters[0], context)?;
                let arg2 = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let (arg1, arg2, language) = some_or_none!(to_argument_compatible_strings(
                        some_or_none!(arg1(tuple)?),
                        some_or_none!(arg2(tuple)?)
                    ));
                    Ok(Some(if let Some(position) = arg1.find(arg2.as_str()) {
                        build_plain_literal(arg1[..position].into(), language)
                    } else {
                        ExpressionTerm::StringLiteral(String::new())
                    }))
                })
            }
            Function::StrAfter => {
                let arg1 = build_expression_evaluator(&parameters[0], context)?;
                let arg2 = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let (arg1, arg2, language) = some_or_none!(to_argument_compatible_strings(
                        some_or_none!(arg1(tuple)?),
                        some_or_none!(arg2(tuple)?)
                    ));
                    Ok(Some(if let Some(position) = arg1.find(arg2.as_str()) {
                        build_plain_literal(arg1[position + arg2.len()..].into(), language)
                    } else {
                        ExpressionTerm::StringLiteral(String::new())
                    }))
                })
            }
            Function::Year => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.year(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DateLiteral(date) => date.year(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearMonthLiteral(year_month) => year_month.year(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearLiteral(year) => year.year(),
                            _ => return Ok(None),
                        }
                        .into(),
                    )))
                })
            }
            Function::Month => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.month(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DateLiteral(date) => date.month(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearMonthLiteral(year_month) => year_month.month(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthDayLiteral(month_day) => month_day.month(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthLiteral(month) => month.month(),
                            _ => return Ok(None),
                        }
                        .into(),
                    )))
                })
            }
            Function::Day => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.day(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DateLiteral(date) => date.day(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthDayLiteral(month_day) => month_day.day(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GDayLiteral(day) => day.day(),
                            _ => return Ok(None),
                        }
                        .into(),
                    )))
                })
            }
            Function::Hours => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.hour(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::TimeLiteral(time) => time.hour(),
                            _ => return Ok(None),
                        }
                        .into(),
                    )))
                })
            }
            Function::Minutes => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::IntegerLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.minute(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::TimeLiteral(time) => time.minute(),
                            _ => return Ok(None),
                        }
                        .into(),
                    )))
                })
            }
            Function::Seconds => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(ExpressionTerm::DecimalLiteral(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.second(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::TimeLiteral(time) => time.second(),
                            _ => return Ok(None),
                        },
                    )))
                })
            }
            Function::Timezone => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok({
                        let result = match some_or_none!(e(tuple)?) {
                            ExpressionTerm::DateTimeLiteral(date_time) => date_time.timezone(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::TimeLiteral(time) => time.timezone(),
                            #[cfg(feature = "sep-0002")]
                            ExpressionTerm::DateLiteral(date) => date.timezone(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearMonthLiteral(year_month) => year_month.timezone(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GYearLiteral(year) => year.timezone(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthDayLiteral(month_day) => month_day.timezone(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GDayLiteral(day) => day.timezone(),
                            #[cfg(feature = "calendar-ext")]
                            ExpressionTerm::GMonthLiteral(month) => month.timezone(),
                            _ => None,
                        };
                        let result = some_or_none!(result);
                        #[cfg(feature = "sep-0002")]
                        {
                            Some(ExpressionTerm::DayTimeDurationLiteral(result))
                        }
                        #[cfg(not(feature = "sep-0002"))]
                        {
                            Some(ExpressionTerm::OtherTypedLiteral {
                                value: result.to_string(),
                                datatype: xsd::DAY_TIME_DURATION.into(),
                            })
                        }
                    })
                })
            }
            Function::Tz => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    let timezone_offset = match some_or_none!(e(tuple)?) {
                        ExpressionTerm::DateTimeLiteral(date_time) => date_time.timezone_offset(),
                        #[cfg(feature = "sep-0002")]
                        ExpressionTerm::TimeLiteral(time) => time.timezone_offset(),
                        #[cfg(feature = "sep-0002")]
                        ExpressionTerm::DateLiteral(date) => date.timezone_offset(),
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GYearMonthLiteral(year_month) => {
                            year_month.timezone_offset()
                        }
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GYearLiteral(year) => year.timezone_offset(),
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GMonthDayLiteral(month_day) => month_day.timezone_offset(),
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GDayLiteral(day) => day.timezone_offset(),
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GMonthLiteral(month) => month.timezone_offset(),
                        _ => return Ok(None),
                    };
                    Ok(Some(ExpressionTerm::StringLiteral(
                        timezone_offset.map_or_else(String::new, |o| o.to_string()),
                    )))
                })
            }
            #[cfg(feature = "sep-0002")]
            Function::Adjust => {
                let dt = build_expression_evaluator(&parameters[0], context)?;
                let tz = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let timezone_offset = Some(some_or_none!(
                        match some_or_none!(tz(tuple)?) {
                            ExpressionTerm::DayTimeDurationLiteral(tz) => {
                                TimezoneOffset::try_from(tz)
                            }
                            ExpressionTerm::DurationLiteral(tz) => TimezoneOffset::try_from(tz),
                            _ => return Ok(None),
                        }
                        .ok()
                    ));
                    Ok(Some(match some_or_none!(dt(tuple)?) {
                        ExpressionTerm::DateTimeLiteral(date_time) => {
                            ExpressionTerm::DateTimeLiteral(some_or_none!(
                                date_time.adjust(timezone_offset)
                            ))
                        }
                        ExpressionTerm::TimeLiteral(time) => {
                            ExpressionTerm::TimeLiteral(some_or_none!(time.adjust(timezone_offset)))
                        }
                        ExpressionTerm::DateLiteral(date) => {
                            ExpressionTerm::DateLiteral(some_or_none!(date.adjust(timezone_offset)))
                        }
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GYearMonthLiteral(year_month) => {
                            ExpressionTerm::GYearMonthLiteral(some_or_none!(
                                year_month.adjust(timezone_offset)
                            ))
                        }
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GYearLiteral(year) => ExpressionTerm::GYearLiteral(
                            some_or_none!(year.adjust(timezone_offset)),
                        ),
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GMonthDayLiteral(month_day) => {
                            ExpressionTerm::GMonthDayLiteral(some_or_none!(
                                month_day.adjust(timezone_offset)
                            ))
                        }
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GDayLiteral(day) => {
                            ExpressionTerm::GDayLiteral(some_or_none!(day.adjust(timezone_offset)))
                        }
                        #[cfg(feature = "calendar-ext")]
                        ExpressionTerm::GMonthLiteral(month) => ExpressionTerm::GMonthLiteral(
                            some_or_none!(month.adjust(timezone_offset)),
                        ),
                        _ => return Ok(None),
                    }))
                })
            }
            Function::Now => {
                let now = context.now();
                Rc::new(move |_| Ok(Some(ExpressionTerm::DateTimeLiteral(now))))
            }
            Function::Uuid => Rc::new(move |_| {
                let mut buffer = String::with_capacity(44);
                buffer.push_str("urn:uuid:");
                generate_uuid(&mut buffer);
                Ok(Some(ExpressionTerm::NamedNode(NamedNode::new_unchecked(
                    buffer,
                ))))
            }),
            Function::StrUuid => Rc::new(move |_| {
                let mut buffer = String::with_capacity(36);
                generate_uuid(&mut buffer);
                Ok(Some(ExpressionTerm::StringLiteral(buffer)))
            }),
            Function::Md5 => build_hash_expression_evaluator::<_, Md5>(parameters, context)?,
            Function::Sha1 => build_hash_expression_evaluator::<_, Sha1>(parameters, context)?,
            Function::Sha256 => build_hash_expression_evaluator::<_, Sha256>(parameters, context)?,
            Function::Sha384 => build_hash_expression_evaluator::<_, Sha384>(parameters, context)?,
            Function::Sha512 => build_hash_expression_evaluator::<_, Sha512>(parameters, context)?,
            Function::StrLang => {
                let lexical_form = build_expression_evaluator(&parameters[0], context)?;
                let lang_tag = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let ExpressionTerm::StringLiteral(value) = some_or_none!(lexical_form(tuple)?)
                    else {
                        return Ok(None);
                    };
                    let ExpressionTerm::StringLiteral(language) = some_or_none!(lang_tag(tuple)?)
                    else {
                        return Ok(None);
                    };
                    Ok(Some(
                        Term::from(some_or_none!(
                            Literal::new_language_tagged_literal(value, language).ok()
                        ))
                        .into(),
                    ))
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::StrLangDir => {
                let lexical_form = build_expression_evaluator(&parameters[0], context)?;
                let lang_tag = build_expression_evaluator(&parameters[1], context)?;
                let direction = build_expression_evaluator(&parameters[2], context)?;
                Rc::new(move |tuple| {
                    let ExpressionTerm::StringLiteral(value) = some_or_none!(lexical_form(tuple)?)
                    else {
                        return Ok(None);
                    };
                    let ExpressionTerm::StringLiteral(language) = some_or_none!(lang_tag(tuple)?)
                    else {
                        return Ok(None);
                    };
                    let ExpressionTerm::StringLiteral(direction) = some_or_none!(direction(tuple)?)
                    else {
                        return Ok(None);
                    };
                    let direction = match direction.as_str() {
                        "ltr" => BaseDirection::Ltr,
                        "rtl" => BaseDirection::Rtl,
                        _ => return Ok(None),
                    };
                    Ok(Some(
                        Term::from(some_or_none!(
                            Literal::new_directional_language_tagged_literal(
                                value, language, direction,
                            )
                            .ok()
                        ))
                        .into(),
                    ))
                })
            }
            Function::StrDt => {
                let lexical_form = build_expression_evaluator(&parameters[0], context)?;
                let datatype = build_expression_evaluator(&parameters[1], context)?;
                Rc::new(move |tuple| {
                    let ExpressionTerm::StringLiteral(value) = some_or_none!(lexical_form(tuple)?)
                    else {
                        return Ok(None);
                    };
                    let ExpressionTerm::NamedNode(datatype) = some_or_none!(datatype(tuple)?)
                    else {
                        return Ok(None);
                    };
                    Ok(Some(
                        Term::from(Literal::new_typed_literal(value, datatype)).into(),
                    ))
                })
            }

            Function::IsIri => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(some_or_none!(e(tuple)?), ExpressionTerm::NamedNode(_)).into(),
                    ))
                })
            }
            Function::IsBlank => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(some_or_none!(e(tuple)?), ExpressionTerm::BlankNode(_)).into(),
                    ))
                })
            }
            Function::IsLiteral => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        match some_or_none!(e(tuple)?) {
                            ExpressionTerm::NamedNode(_) | ExpressionTerm::BlankNode(_) => false,
                            #[cfg(feature = "sparql-12")]
                            ExpressionTerm::Triple(_) => false,
                            _ => true,
                        }
                        .into(),
                    ))
                })
            }
            Function::IsNumeric => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(
                            some_or_none!(e(tuple)?),
                            ExpressionTerm::IntegerLiteral(_)
                                | ExpressionTerm::DecimalLiteral(_)
                                | ExpressionTerm::FloatLiteral(_)
                                | ExpressionTerm::DoubleLiteral(_)
                        )
                        .into(),
                    ))
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::HasLang => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(
                            some_or_none!(e(tuple)?),
                            ExpressionTerm::LangStringLiteral { .. }
                                | ExpressionTerm::DirLangStringLiteral { .. }
                        )
                        .into(),
                    ))
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::HasLangDir => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(
                            some_or_none!(e(tuple)?),
                            ExpressionTerm::DirLangStringLiteral { .. }
                        )
                        .into(),
                    ))
                })
            }
            Function::Regex => {
                let cancellation = context.cancellation_token().unwrap_or_default();
                let text = build_expression_evaluator(&parameters[0], context)?;
                if let Some(regex) = static_pattern_if_exists(&parameters[1], parameters.get(2)) {
                    Rc::new(move |tuple| {
                        let regex =
                            some_or_none!(regex.get_with_cancel(&|| cancellation.is_cancelled())?);
                        let (text, _) =
                            some_or_none!(to_string_and_language(some_or_none!(text(tuple)?)));
                        Ok(Some(
                            regex
                                .try_is_match_with_cancel(&text, RegexLimits::default(), &|| {
                                    cancellation.is_cancelled()
                                })?
                                .into(),
                        ))
                    })
                } else {
                    let pattern = build_expression_evaluator(&parameters[1], context)?;
                    let flags = parameters
                        .get(2)
                        .map(|flags| build_expression_evaluator(flags, context))
                        .transpose()?;
                    Rc::new(move |tuple| {
                        let ExpressionTerm::StringLiteral(pattern) = some_or_none!(pattern(tuple)?)
                        else {
                            return Ok(None);
                        };
                        let options = if let Some(flags) = &flags {
                            let ExpressionTerm::StringLiteral(options) =
                                some_or_none!(flags(tuple)?)
                            else {
                                return Ok(None);
                            };
                            Some(options)
                        } else {
                            None
                        };
                        let regex = some_or_none!(compile_pattern_with_cancel(
                            &pattern,
                            options.as_deref(),
                            &|| cancellation.is_cancelled()
                        )?);
                        let (text, _) =
                            some_or_none!(to_string_and_language(some_or_none!(text(tuple)?)));
                        Ok(Some(
                            regex
                                .try_is_match_with_cancel(&text, RegexLimits::default(), &|| {
                                    cancellation.is_cancelled()
                                })?
                                .into(),
                        ))
                    })
                }
            }
            #[cfg(feature = "sparql-12")]
            Function::Triple => {
                let s = build_expression_evaluator(&parameters[0], context)?;
                let p = build_expression_evaluator(&parameters[1], context)?;
                let o = build_expression_evaluator(&parameters[2], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        some_or_none!(ExpressionTriple::new(
                            some_or_none!(s(tuple)?),
                            some_or_none!(p(tuple)?),
                            some_or_none!(o(tuple)?)
                        ))
                        .into(),
                    ))
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::Subject => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(
                        if let ExpressionTerm::Triple(t) = some_or_none!(e(tuple)?) {
                            Some(t.subject.into())
                        } else {
                            None
                        },
                    )
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::Predicate => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(
                        if let ExpressionTerm::Triple(t) = some_or_none!(e(tuple)?) {
                            Some(t.predicate.into())
                        } else {
                            None
                        },
                    )
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::Object => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(
                        if let ExpressionTerm::Triple(t) = some_or_none!(e(tuple)?) {
                            Some(t.object)
                        } else {
                            None
                        },
                    )
                })
            }
            #[cfg(feature = "sparql-12")]
            Function::IsTriple => {
                let e = build_expression_evaluator(&parameters[0], context)?;
                Rc::new(move |tuple| {
                    Ok(Some(
                        matches!(some_or_none!(e(tuple)?), ExpressionTerm::Triple(_)).into(),
                    ))
                })
            }
            Function::Custom(function_name) => {
                if let Some(function) = context.custom_functions().get(function_name).cloned() {
                    let args = parameters
                        .iter()
                        .map(|e| build_expression_evaluator(e, context))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(Rc::new(move |tuple| {
                        let mut values = Vec::with_capacity(args.len());
                        for argument in &args {
                            values.push(some_or_none!(argument(tuple)?).into());
                        }
                        Ok(Some(some_or_none!(function(&values)).into()))
                    }));
                }

                macro_rules! cast_fn {
                    ($eval:expr) => {{
                        if parameters.len() != 1 {
                            return Err(
                                ExpressionEvaluationError::UnsupportedCustomFunctionArity {
                                    name: function_name.clone(),
                                    expected: 1..=1,
                                    actual: parameters.len(),
                                },
                            );
                        }
                        let e = build_expression_evaluator(&parameters[0], context)?;
                        Rc::new(move |tuple| Ok(($eval)(some_or_none!(e(tuple)?))))
                    }};
                }

                match function_name.as_ref() {
                    xsd::STRING => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::StringLiteral(
                            match t.into() {
                                Term::NamedNode(term) => term.into_string(),
                                Term::BlankNode(_) => return None,
                                Term::Literal(term) => term.destruct().0,
                                #[cfg(feature = "sparql-12")]
                                Term::Triple(_) => return None,
                            }
                        )))
                    }
                    xsd::BOOLEAN => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::BooleanLiteral(
                            match t {
                                ExpressionTerm::BooleanLiteral(value) => value,
                                ExpressionTerm::FloatLiteral(value) => value.into(),
                                ExpressionTerm::DoubleLiteral(value) => value.into(),
                                ExpressionTerm::IntegerLiteral(value) => value.into(),
                                ExpressionTerm::DecimalLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    xsd::DOUBLE => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::DoubleLiteral(match t {
                                ExpressionTerm::FloatLiteral(value) => value.into(),
                                ExpressionTerm::DoubleLiteral(value) => value,
                                ExpressionTerm::IntegerLiteral(value) => value.into(),
                                ExpressionTerm::DecimalLiteral(value) => value.into(),
                                ExpressionTerm::BooleanLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    xsd::FLOAT => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::FloatLiteral(match t {
                                ExpressionTerm::FloatLiteral(value) => value,
                                ExpressionTerm::DoubleLiteral(value) => value.into(),
                                ExpressionTerm::IntegerLiteral(value) => value.into(),
                                ExpressionTerm::DecimalLiteral(value) => value.into(),
                                ExpressionTerm::BooleanLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    xsd::INTEGER => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::IntegerLiteral(
                            match t {
                                ExpressionTerm::FloatLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::DoubleLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::IntegerLiteral(value) => value,
                                ExpressionTerm::DecimalLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::BooleanLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    xsd::DECIMAL => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::DecimalLiteral(
                            match t {
                                ExpressionTerm::FloatLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::DoubleLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::IntegerLiteral(value) => value.into(),
                                ExpressionTerm::DecimalLiteral(value) => value,
                                ExpressionTerm::BooleanLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    #[cfg(feature = "sep-0002")]
                    xsd::DATE => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::DateLiteral(match t {
                                ExpressionTerm::DateLiteral(value) => value,
                                ExpressionTerm::DateTimeLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    #[cfg(feature = "sep-0002")]
                    xsd::TIME => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::TimeLiteral(match t {
                                ExpressionTerm::TimeLiteral(value) => value,
                                ExpressionTerm::DateTimeLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    xsd::DATE_TIME => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::DateTimeLiteral(
                            match t {
                                ExpressionTerm::DateTimeLiteral(value) => value,
                                #[cfg(feature = "sep-0002")]
                                ExpressionTerm::DateLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    #[cfg(feature = "sep-0002")]
                    xsd::DURATION => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::DurationLiteral(
                            match t {
                                ExpressionTerm::DurationLiteral(value) => value,
                                ExpressionTerm::YearMonthDurationLiteral(value) => value.into(),
                                ExpressionTerm::DayTimeDurationLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    #[cfg(feature = "sep-0002")]
                    xsd::YEAR_MONTH_DURATION => {
                        cast_fn!(|t: ExpressionTerm| Some(
                            ExpressionTerm::YearMonthDurationLiteral(match t {
                                ExpressionTerm::DurationLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::YearMonthDurationLiteral(value) => value,
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            })
                        ))
                    }
                    #[cfg(feature = "sep-0002")]
                    xsd::DAY_TIME_DURATION => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::DayTimeDurationLiteral(
                                match t {
                                    ExpressionTerm::DurationLiteral(value) =>
                                        value.try_into().ok()?,
                                    ExpressionTerm::DayTimeDurationLiteral(value) => value,
                                    ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                    _ => return None,
                                }
                            ))
                        )
                    }
                    #[cfg(feature = "calendar-ext")]
                    xsd::G_YEAR => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::GYearLiteral(match t {
                                ExpressionTerm::GYearLiteral(value) => value,
                                ExpressionTerm::GYearMonthLiteral(value) => {
                                    value.try_into().ok()?
                                }
                                ExpressionTerm::DateLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::DateTimeLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    #[cfg(feature = "calendar-ext")]
                    xsd::G_YEAR_MONTH => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::GYearMonthLiteral(
                            match t {
                                ExpressionTerm::GYearMonthLiteral(value) => value,
                                ExpressionTerm::DateLiteral(value) => value.into(),
                                ExpressionTerm::DateTimeLiteral(value) => value.try_into().ok()?,
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    #[cfg(feature = "calendar-ext")]
                    xsd::G_MONTH => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::GMonthLiteral(match t {
                                ExpressionTerm::GMonthLiteral(value) => value,
                                ExpressionTerm::GYearMonthLiteral(value) => value.into(),
                                ExpressionTerm::GMonthDayLiteral(value) => value.into(),
                                ExpressionTerm::DateLiteral(value) => value.into(),
                                ExpressionTerm::DateTimeLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    #[cfg(feature = "calendar-ext")]
                    xsd::G_MONTH_DAY => {
                        cast_fn!(|t: ExpressionTerm| Some(ExpressionTerm::GMonthDayLiteral(
                            match t {
                                ExpressionTerm::GMonthDayLiteral(value) => value,
                                ExpressionTerm::DateLiteral(value) => value.into(),
                                ExpressionTerm::DateTimeLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }
                        )))
                    }
                    #[cfg(feature = "calendar-ext")]
                    xsd::G_DAY => {
                        cast_fn!(
                            |t: ExpressionTerm| Some(ExpressionTerm::GDayLiteral(match t {
                                ExpressionTerm::GDayLiteral(value) => value,
                                ExpressionTerm::GMonthDayLiteral(value) => value.into(),
                                ExpressionTerm::DateLiteral(value) => value.into(),
                                ExpressionTerm::DateTimeLiteral(value) => value.into(),
                                ExpressionTerm::StringLiteral(value) => value.parse().ok()?,
                                _ => return None,
                            }))
                        )
                    }
                    _ => {
                        return Err(ExpressionEvaluationError::UnsupportedCustomFunction(
                            function_name.clone(),
                        ));
                    }
                }
            }
        },
    })
}

/// Evaluates an expression and returns an internal term
///
/// Returns None if building such expression implies to convert into an [`ExpressionTerm`] then back.
pub fn try_build_internal_expression_evaluator<'a, C: ExpressionEvaluatorContext<'a>>(
    expression: &Expression,
    context: &mut C,
) -> Result<
    Option<ExpressionEvaluator<'a, C::Tuple, C::Term, C::Error>>,
    ExpressionEvaluationError<C::Error>,
> {
    Ok(Some(match expression {
        Expression::NamedNode(t) => {
            let t = context
                .internalize_named_node(t)
                .map_err(ExpressionEvaluationError::Context)?;
            Rc::new(move |_| Ok(Some(t.clone())))
        }
        Expression::Literal(t) => {
            let t = context
                .internalize_literal(t)
                .map_err(ExpressionEvaluationError::Context)?;
            Rc::new(move |_| Ok(Some(t.clone())))
        }
        Expression::Variable(v) => {
            let lookup = context.build_variable_lookup(v);
            Rc::new(move |tuple| Ok(lookup(tuple)))
        }
        Expression::Coalesce(l) => {
            let Some(l) = l
                .iter()
                .map(|e| try_build_internal_expression_evaluator(e, context))
                .collect::<Result<Option<Vec<_>>, _>>()?
            else {
                return Ok(None);
            };
            Rc::new(move |tuple| {
                for e in &l {
                    if let Some(result) = e(tuple)? {
                        return Ok(Some(result));
                    }
                }
                Ok(None)
            })
        }
        Expression::If(a, b, c) => {
            let a = build_expression_evaluator(a, context)?;
            let Some(b) = try_build_internal_expression_evaluator(b, context)? else {
                return Ok(None);
            };
            let Some(c) = try_build_internal_expression_evaluator(c, context)? else {
                return Ok(None);
            };
            Rc::new(move |tuple| {
                Ok(
                    if some_or_none!(some_or_none!(a(tuple)?).effective_boolean_value()) {
                        b(tuple)?
                    } else {
                        c(tuple)?
                    },
                )
            })
        }
        _ => return Ok(None),
    }))
}

fn build_hash_expression_evaluator<'a, C: ExpressionEvaluatorContext<'a>, H: Digest>(
    parameters: &[Expression],
    context: &mut C,
) -> Result<
    ExpressionEvaluator<'a, C::Tuple, ExpressionTerm, C::Error>,
    ExpressionEvaluationError<C::Error>,
> {
    let arg = build_expression_evaluator(&parameters[0], context)?;
    Ok(Rc::new(move |tuple| {
        let ExpressionTerm::StringLiteral(input) = some_or_none!(arg(tuple)?) else {
            return Ok(None);
        };
        let hash = hex::encode(H::new().chain_update(input.as_str()).finalize());
        Ok(Some(ExpressionTerm::StringLiteral(hash)))
    }))
}

#[cfg(feature = "sparql-12")]
type LanguageWithMaybeBaseDirection = (String, Option<BaseDirection>);
#[cfg(not(feature = "sparql-12"))]
type LanguageWithMaybeBaseDirection = String;

#[cfg(feature = "sparql-12")]
fn to_string_and_language(
    term: ExpressionTerm,
) -> Option<(String, Option<LanguageWithMaybeBaseDirection>)> {
    match term {
        ExpressionTerm::StringLiteral(value) => Some((value, None)),
        ExpressionTerm::LangStringLiteral { value, language } => {
            Some((value, Some((language, None))))
        }
        ExpressionTerm::DirLangStringLiteral {
            value,
            language,
            direction,
        } => Some((value, Some((language, Some(direction))))),
        _ => None,
    }
}

#[cfg(not(feature = "sparql-12"))]
fn to_string_and_language(
    term: ExpressionTerm,
) -> Option<(String, Option<LanguageWithMaybeBaseDirection>)> {
    match term {
        ExpressionTerm::StringLiteral(value) => Some((value, None)),
        ExpressionTerm::LangStringLiteral { value, language } => Some((value, Some(language))),
        _ => None,
    }
}

#[cfg(feature = "sparql-12")]
fn build_plain_literal(
    value: String,
    language: Option<LanguageWithMaybeBaseDirection>,
) -> ExpressionTerm {
    if let Some((language, direction)) = language {
        if let Some(direction) = direction {
            ExpressionTerm::DirLangStringLiteral {
                value,
                language,
                direction,
            }
        } else {
            ExpressionTerm::LangStringLiteral { value, language }
        }
    } else {
        ExpressionTerm::StringLiteral(value)
    }
}

#[cfg(not(feature = "sparql-12"))]
fn build_plain_literal(
    value: String,
    language: Option<LanguageWithMaybeBaseDirection>,
) -> ExpressionTerm {
    if let Some(language) = language {
        ExpressionTerm::LangStringLiteral { value, language }
    } else {
        ExpressionTerm::StringLiteral(value)
    }
}

fn to_argument_compatible_strings(
    arg1: ExpressionTerm,
    arg2: ExpressionTerm,
) -> Option<(String, String, Option<LanguageWithMaybeBaseDirection>)> {
    let (value1, language1) = to_string_and_language(arg1)?;
    let (value2, language2) = to_string_and_language(arg2)?;
    (language2.is_none() || language1 == language2).then_some((value1, value2, language1))
}

// Each expression owns its cache. In particular, building an unselected IF or
// COALESCE branch must not compile its pattern or raise its resource failure.
struct StaticPattern {
    pattern: String,
    options: Option<String>,
    compiled: OnceCell<Result<Option<Regex>, RegexError>>,
}

impl StaticPattern {
    #[cfg(test)]
    fn get<C>(&self) -> Result<Option<&Regex>, ExpressionEvaluationError<C>> {
        self.get_with_cancel(&|| false)
    }

    fn get_with_cancel<C>(
        &self,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<&Regex>, ExpressionEvaluationError<C>> {
        if cancelled() {
            return Err(ExpressionEvaluationError::Cancelled);
        }
        match self.compiled.get_or_init(|| {
            compile_pattern_with_cancel(&self.pattern, self.options.as_deref(), cancelled)
        }) {
            Ok(regex) => Ok(regex.as_ref()),
            Err(error) => Err(error.clone().into()),
        }
    }
}

fn static_pattern_if_exists(
    pattern: &Expression,
    options: Option<&Expression>,
) -> Option<StaticPattern> {
    let Expression::Literal(pattern) = pattern else {
        return None;
    };
    if pattern.datatype() != xsd::STRING {
        return None;
    }
    let options = if let Some(options) = options {
        let Expression::Literal(options) = options else {
            return None;
        };
        if options.datatype() != xsd::STRING {
            return None;
        }
        Some(options.value().to_owned())
    } else {
        None
    };
    Some(StaticPattern {
        pattern: pattern.value().to_owned(),
        options,
        compiled: OnceCell::new(),
    })
}

#[cfg(test)]
fn compile_pattern(pattern: &str, flags: Option<&str>) -> Result<Option<Regex>, RegexError> {
    compile_pattern_with_cancel(pattern, flags, &|| false)
}

fn compile_pattern_with_cancel(
    pattern: &str,
    flags: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Regex>, RegexError> {
    match Regex::xpath_with_cancel(
        pattern,
        flags.unwrap_or_default(),
        XsdVersion::V1_1,
        RegexLimits::default(),
        cancelled,
    ) {
        Ok(regex) => Ok(Some(regex)),
        Err(error) if error.forx_code().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

// The vendored replacement parser preserves the checked linear capture-prefix
// algorithm from D3FEND's original repair, now with fallible matching/output.
fn replace_xpath_with_cancel(
    regex: &Regex,
    text: &str,
    replacement: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<String>, RegexError> {
    match regex.replace_with_cancel(text, replacement, RegexLimits::default(), cancelled) {
        Ok(replaced) => Ok(Some(replaced)),
        Err(error) if error.forx_code().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Equality operator (=)
fn equals(a: &ExpressionTerm, b: &ExpressionTerm) -> Option<bool> {
    match a {
        ExpressionTerm::NamedNode(_)
        | ExpressionTerm::BlankNode(_)
        | ExpressionTerm::LangStringLiteral { .. } => Some(a == b),
        #[cfg(feature = "sparql-12")]
        ExpressionTerm::DirLangStringLiteral { .. } => Some(a == b),
        ExpressionTerm::StringLiteral(a) => match b {
            ExpressionTerm::StringLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::OtherTypedLiteral { .. } => match b {
            ExpressionTerm::OtherTypedLiteral { .. } if a == b => Some(true),
            ExpressionTerm::NamedNode(_)
            | ExpressionTerm::BlankNode(_)
            | ExpressionTerm::LangStringLiteral { .. } => Some(false),
            #[cfg(feature = "sparql-12")]
            ExpressionTerm::DirLangStringLiteral { .. } => Some(false),
            #[cfg(feature = "sparql-12")]
            ExpressionTerm::Triple(_) => Some(false),
            _ => None,
        },
        ExpressionTerm::BooleanLiteral(a) => match b {
            ExpressionTerm::BooleanLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::FloatLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Some(a == b),
            ExpressionTerm::DoubleLiteral(b) => Some(Double::from(*a) == *b),
            ExpressionTerm::IntegerLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::DecimalLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::DoubleLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::DoubleLiteral(b) => Some(a == b),
            ExpressionTerm::IntegerLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::DecimalLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::IntegerLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Some(Float::from(*a) == *b),
            ExpressionTerm::DoubleLiteral(b) => Some(Double::from(*a) == *b),
            ExpressionTerm::IntegerLiteral(b) => Some(a == b),
            ExpressionTerm::DecimalLiteral(b) => Some(Decimal::from(*a) == *b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::DecimalLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Some(Float::from(*a) == *b),
            ExpressionTerm::DoubleLiteral(b) => Some(Double::from(*a) == *b),
            ExpressionTerm::IntegerLiteral(b) => Some(*a == (*b).into()),
            ExpressionTerm::DecimalLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        ExpressionTerm::DateTimeLiteral(a) => match b {
            ExpressionTerm::DateTimeLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::TimeLiteral(a) => match b {
            ExpressionTerm::TimeLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DateLiteral(a) => match b {
            ExpressionTerm::DateLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GYearMonthLiteral(a) => match b {
            ExpressionTerm::GYearMonthLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GYearLiteral(a) => match b {
            ExpressionTerm::GYearLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GMonthDayLiteral(a) => match b {
            ExpressionTerm::GMonthDayLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GDayLiteral(a) => match b {
            ExpressionTerm::GDayLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GMonthLiteral(a) => match b {
            ExpressionTerm::GMonthLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => Some(a == b),
            ExpressionTerm::YearMonthDurationLiteral(b) => Some(a == b),
            ExpressionTerm::DayTimeDurationLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::YearMonthDurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => Some(a == b),
            ExpressionTerm::YearMonthDurationLiteral(b) => Some(a == b),
            ExpressionTerm::DayTimeDurationLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DayTimeDurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => Some(a == b),
            ExpressionTerm::YearMonthDurationLiteral(b) => Some(a == b),
            ExpressionTerm::DayTimeDurationLiteral(b) => Some(a == b),
            ExpressionTerm::OtherTypedLiteral { .. } => None,
            _ => Some(false),
        },
        #[cfg(feature = "sparql-12")]
        ExpressionTerm::Triple(a) => {
            if let ExpressionTerm::Triple(b) = b {
                triple_equals(a, b)
            } else {
                Some(false)
            }
        }
    }
}

#[cfg(feature = "sparql-12")]
fn triple_equals(a: &ExpressionTriple, b: &ExpressionTriple) -> Option<bool> {
    Some(a.subject == b.subject && a.predicate == b.predicate && equals(&a.object, &b.object)?)
}

/// Comparison for <, >, <= and >= operators
fn partial_cmp(a: &ExpressionTerm, b: &ExpressionTerm) -> Option<Ordering> {
    if a == b {
        return Some(Ordering::Equal);
    }
    #[cfg(feature = "sparql-12")]
    if let ExpressionTerm::Triple(a) = a {
        return if let ExpressionTerm::Triple(b) = b {
            partial_cmp_triples(a, b)
        } else {
            None
        };
    }
    partial_cmp_literals(a, b)
}

pub fn partial_cmp_literals(a: &ExpressionTerm, b: &ExpressionTerm) -> Option<Ordering> {
    match a {
        ExpressionTerm::StringLiteral(a) => {
            if let ExpressionTerm::StringLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        ExpressionTerm::LangStringLiteral {
            value: va,
            language: la,
        } => {
            if let ExpressionTerm::LangStringLiteral {
                value: vb,
                language: lb,
            } = b
            {
                if la == lb { va.partial_cmp(vb) } else { None }
            } else {
                None
            }
        }
        #[cfg(feature = "sparql-12")]
        ExpressionTerm::DirLangStringLiteral {
            value: va,
            language: la,
            direction: da,
        } => {
            if let ExpressionTerm::DirLangStringLiteral {
                value: vb,
                language: lb,
                direction: db,
            } = b
            {
                if la == lb && da == db {
                    va.partial_cmp(vb)
                } else {
                    None
                }
            } else {
                None
            }
        }
        ExpressionTerm::FloatLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::DoubleLiteral(b) => Double::from(*a).partial_cmp(b),
            ExpressionTerm::IntegerLiteral(b) => a.partial_cmp(&Float::from(*b)),
            ExpressionTerm::DecimalLiteral(b) => a.partial_cmp(&(*b).into()),
            _ => None,
        },
        ExpressionTerm::DoubleLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => a.partial_cmp(&(*b).into()),
            ExpressionTerm::DoubleLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::IntegerLiteral(b) => a.partial_cmp(&Double::from(*b)),
            ExpressionTerm::DecimalLiteral(b) => a.partial_cmp(&(*b).into()),
            _ => None,
        },
        ExpressionTerm::IntegerLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Float::from(*a).partial_cmp(b),
            ExpressionTerm::DoubleLiteral(b) => Double::from(*a).partial_cmp(b),
            ExpressionTerm::IntegerLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::DecimalLiteral(b) => Decimal::from(*a).partial_cmp(b),
            _ => None,
        },
        ExpressionTerm::DecimalLiteral(a) => match b {
            ExpressionTerm::FloatLiteral(b) => Float::from(*a).partial_cmp(b),
            ExpressionTerm::DoubleLiteral(b) => Double::from(*a).partial_cmp(b),
            ExpressionTerm::IntegerLiteral(b) => a.partial_cmp(&Decimal::from(*b)),
            ExpressionTerm::DecimalLiteral(b) => a.partial_cmp(b),
            _ => None,
        },
        ExpressionTerm::DateTimeLiteral(a) => {
            if let ExpressionTerm::DateTimeLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::TimeLiteral(a) => {
            if let ExpressionTerm::TimeLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DateLiteral(a) => {
            if let ExpressionTerm::DateLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GYearMonthLiteral(a) => {
            if let ExpressionTerm::GYearMonthLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GYearLiteral(a) => {
            if let ExpressionTerm::GYearLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GMonthDayLiteral(a) => {
            if let ExpressionTerm::GMonthDayLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GDayLiteral(a) => {
            if let ExpressionTerm::GDayLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "calendar-ext")]
        ExpressionTerm::GMonthLiteral(a) => {
            if let ExpressionTerm::GMonthLiteral(b) = b {
                a.partial_cmp(b)
            } else {
                None
            }
        }
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::YearMonthDurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::DayTimeDurationLiteral(b) => a.partial_cmp(b),
            _ => None,
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::YearMonthDurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::YearMonthDurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::DayTimeDurationLiteral(b) => a.partial_cmp(b),
            _ => None,
        },
        #[cfg(feature = "sep-0002")]
        ExpressionTerm::DayTimeDurationLiteral(a) => match b {
            ExpressionTerm::DurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::YearMonthDurationLiteral(b) => a.partial_cmp(b),
            ExpressionTerm::DayTimeDurationLiteral(b) => a.partial_cmp(b),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(feature = "sparql-12")]
fn partial_cmp_triples(a: &ExpressionTriple, b: &ExpressionTriple) -> Option<Ordering> {
    // We compare subjects
    match (&a.subject, &b.subject) {
        (NamedOrBlankNode::NamedNode(a), NamedOrBlankNode::NamedNode(b)) => {
            if a != b {
                return None;
            }
        }
        (NamedOrBlankNode::BlankNode(a), NamedOrBlankNode::BlankNode(b)) => {
            if a != b {
                return None;
            }
        }
        _ => return None,
    }
    if a.predicate != b.predicate {
        return None;
    }
    partial_cmp(&a.object, &b.object)
}

pub enum NumericBinaryOperands {
    Float(Float, Float),
    Double(Double, Double),
    Integer(Integer, Integer),
    Decimal(Decimal, Decimal),
    #[cfg(feature = "sep-0002")]
    Duration(Duration, Duration),
    #[cfg(feature = "sep-0002")]
    YearMonthDuration(YearMonthDuration, YearMonthDuration),
    #[cfg(feature = "sep-0002")]
    DayTimeDuration(DayTimeDuration, DayTimeDuration),
    #[cfg(feature = "sep-0002")]
    DateTime(DateTime, DateTime),
    #[cfg(feature = "sep-0002")]
    Time(Time, Time),
    #[cfg(feature = "sep-0002")]
    Date(Date, Date),
    #[cfg(feature = "sep-0002")]
    DateTimeDuration(DateTime, Duration),
    #[cfg(feature = "sep-0002")]
    DateTimeYearMonthDuration(DateTime, YearMonthDuration),
    #[cfg(feature = "sep-0002")]
    DateTimeDayTimeDuration(DateTime, DayTimeDuration),
    #[cfg(feature = "sep-0002")]
    DateDuration(Date, Duration),
    #[cfg(feature = "sep-0002")]
    DateYearMonthDuration(Date, YearMonthDuration),
    #[cfg(feature = "sep-0002")]
    DateDayTimeDuration(Date, DayTimeDuration),
    #[cfg(feature = "sep-0002")]
    TimeDuration(Time, Duration),
    #[cfg(feature = "sep-0002")]
    TimeDayTimeDuration(Time, DayTimeDuration),
}

impl NumericBinaryOperands {
    pub fn new(a: ExpressionTerm, b: ExpressionTerm) -> Option<Self> {
        match (a, b) {
            (ExpressionTerm::FloatLiteral(v1), ExpressionTerm::FloatLiteral(v2)) => {
                Some(Self::Float(v1, v2))
            }
            (ExpressionTerm::FloatLiteral(v1), ExpressionTerm::DoubleLiteral(v2)) => {
                Some(Self::Double(v1.into(), v2))
            }
            (ExpressionTerm::FloatLiteral(v1), ExpressionTerm::IntegerLiteral(v2)) => {
                Some(Self::Float(v1, v2.into()))
            }
            (ExpressionTerm::FloatLiteral(v1), ExpressionTerm::DecimalLiteral(v2)) => {
                Some(Self::Float(v1, v2.into()))
            }
            (ExpressionTerm::DoubleLiteral(v1), ExpressionTerm::FloatLiteral(v2)) => {
                Some(Self::Double(v1, v2.into()))
            }
            (ExpressionTerm::DoubleLiteral(v1), ExpressionTerm::DoubleLiteral(v2)) => {
                Some(Self::Double(v1, v2))
            }
            (ExpressionTerm::DoubleLiteral(v1), ExpressionTerm::IntegerLiteral(v2)) => {
                Some(Self::Double(v1, v2.into()))
            }
            (ExpressionTerm::DoubleLiteral(v1), ExpressionTerm::DecimalLiteral(v2)) => {
                Some(Self::Double(v1, v2.into()))
            }
            (ExpressionTerm::IntegerLiteral(v1), ExpressionTerm::FloatLiteral(v2)) => {
                Some(Self::Float(v1.into(), v2))
            }
            (ExpressionTerm::IntegerLiteral(v1), ExpressionTerm::DoubleLiteral(v2)) => {
                Some(Self::Double(v1.into(), v2))
            }
            (ExpressionTerm::IntegerLiteral(v1), ExpressionTerm::IntegerLiteral(v2)) => {
                Some(Self::Integer(v1, v2))
            }
            (ExpressionTerm::IntegerLiteral(v1), ExpressionTerm::DecimalLiteral(v2)) => {
                Some(Self::Decimal(v1.into(), v2))
            }
            (ExpressionTerm::DecimalLiteral(v1), ExpressionTerm::FloatLiteral(v2)) => {
                Some(Self::Float(v1.into(), v2))
            }
            (ExpressionTerm::DecimalLiteral(v1), ExpressionTerm::DoubleLiteral(v2)) => {
                Some(Self::Double(v1.into(), v2))
            }
            (ExpressionTerm::DecimalLiteral(v1), ExpressionTerm::IntegerLiteral(v2)) => {
                Some(Self::Decimal(v1, v2.into()))
            }
            (ExpressionTerm::DecimalLiteral(v1), ExpressionTerm::DecimalLiteral(v2)) => {
                Some(Self::Decimal(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DurationLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::Duration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DurationLiteral(v1), ExpressionTerm::YearMonthDurationLiteral(v2)) => {
                Some(Self::Duration(v1, v2.into()))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DurationLiteral(v1), ExpressionTerm::DayTimeDurationLiteral(v2)) => {
                Some(Self::Duration(v1, v2.into()))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::YearMonthDurationLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::Duration(v1.into(), v2))
            }
            #[cfg(feature = "sep-0002")]
            (
                ExpressionTerm::YearMonthDurationLiteral(v1),
                ExpressionTerm::YearMonthDurationLiteral(v2),
            ) => Some(Self::YearMonthDuration(v1, v2)),
            #[cfg(feature = "sep-0002")]
            (
                ExpressionTerm::YearMonthDurationLiteral(v1),
                ExpressionTerm::DayTimeDurationLiteral(v2),
            ) => Some(Self::Duration(v1.into(), v2.into())),
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DayTimeDurationLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::Duration(v1.into(), v2))
            }
            #[cfg(feature = "sep-0002")]
            (
                ExpressionTerm::DayTimeDurationLiteral(v1),
                ExpressionTerm::YearMonthDurationLiteral(v2),
            ) => Some(Self::Duration(v1.into(), v2.into())),
            #[cfg(feature = "sep-0002")]
            (
                ExpressionTerm::DayTimeDurationLiteral(v1),
                ExpressionTerm::DayTimeDurationLiteral(v2),
            ) => Some(Self::DayTimeDuration(v1, v2)),
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateTimeLiteral(v1), ExpressionTerm::DateTimeLiteral(v2)) => {
                Some(Self::DateTime(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateLiteral(v1), ExpressionTerm::DateLiteral(v2)) => {
                Some(Self::Date(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::TimeLiteral(v1), ExpressionTerm::TimeLiteral(v2)) => {
                Some(Self::Time(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateTimeLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::DateTimeDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateTimeLiteral(v1), ExpressionTerm::YearMonthDurationLiteral(v2)) => {
                Some(Self::DateTimeYearMonthDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateTimeLiteral(v1), ExpressionTerm::DayTimeDurationLiteral(v2)) => {
                Some(Self::DateTimeDayTimeDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::DateDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateLiteral(v1), ExpressionTerm::YearMonthDurationLiteral(v2)) => {
                Some(Self::DateYearMonthDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::DateLiteral(v1), ExpressionTerm::DayTimeDurationLiteral(v2)) => {
                Some(Self::DateDayTimeDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::TimeLiteral(v1), ExpressionTerm::DurationLiteral(v2)) => {
                Some(Self::TimeDuration(v1, v2))
            }
            #[cfg(feature = "sep-0002")]
            (ExpressionTerm::TimeLiteral(v1), ExpressionTerm::DayTimeDurationLiteral(v2)) => {
                Some(Self::TimeDayTimeDuration(v1, v2))
            }
            _ => None,
        }
    }
}

struct ZipLongest<T1, T2, I1: Iterator<Item = T1>, I2: Iterator<Item = T2>> {
    a: I1,
    b: I2,
}

impl<T1, T2, I1: Iterator<Item = T1>, I2: Iterator<Item = T2>> ZipLongest<T1, T2, I1, I2> {
    fn new(a: I1, b: I2) -> Self {
        Self { a, b }
    }
}

impl<T1, T2, I1: Iterator<Item = T1>, I2: Iterator<Item = T2>> Iterator
    for ZipLongest<T1, T2, I1, I2>
{
    type Item = (Option<T1>, Option<T2>);

    fn next(&mut self) -> Option<Self::Item> {
        match (self.a.next(), self.b.next()) {
            (None, None) => None,
            r => Some(r),
        }
    }
}

fn generate_uuid(buffer: &mut String) {
    let mut uuid = random::<u128>().to_le_bytes();
    uuid[6] = (uuid[6] & 0x0F) | 0x40;
    uuid[8] = (uuid[8] & 0x3F) | 0x80;

    write_hexa_bytes(&uuid[0..4], buffer);
    buffer.push('-');
    write_hexa_bytes(&uuid[4..6], buffer);
    buffer.push('-');
    write_hexa_bytes(&uuid[6..8], buffer);
    buffer.push('-');
    write_hexa_bytes(&uuid[8..10], buffer);
    buffer.push('-');
    write_hexa_bytes(&uuid[10..16], buffer);
}

fn write_hexa_bytes(bytes: &[u8], buffer: &mut String) {
    for b in bytes {
        let high = b / 16;
        buffer.push(char::from(if high < 10 {
            b'0' + high
        } else {
            b'a' + (high - 10)
        }));
        let low = b % 16;
        buffer.push(char::from(if low < 10 {
            b'0' + low
        } else {
            b'a' + (low - 10)
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpath_replace_replacement_language_and_errors() {
        for (text, pattern, replacement, flags, expected) in [
            ("abc", "b", "d", "", Some("adc")),
            ("abc", "b", "", "", Some("ac")),
            ("abc", "a*", "X", "", None),
            ("abc", "", "X", "q", None),
            ("nomatch", "b", "$", "", None),
            ("nomatch", "b", "\\q", "", None),
            ("abc", "b", "\\", "", None),
            ("abc", "b", "$x", "", None),
            ("abc", "b", "\\$", "", Some("a$c")),
            ("abc", "b", "\\\\", "", Some("a\\c")),
            ("ab", "(a)(b)", "$23:$1:$0", "", Some("b3:a:ab")),
            ("a", "(a)", "\\$$1", "", Some("$a")),
            ("ab", "(?:a)(b)", "$1:$2", "", Some("b:")),
            ("b", "(a)?b", "[$1][$0]", "", Some("[][b]")),
            ("a", "(a)", "$010|$099|$000|$2", "", Some("a0|9|a|")),
            (
                "abcdefghij",
                "(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)",
                "$10|$11|$19|$99",
                "",
                Some("j|a1|a9|i9"),
            ),
            (
                "a",
                "(a)",
                "$000000000000000000000000000000000000000000000000001",
                "",
                Some("a"),
            ),
            ("abc", "b", "$0", "q", Some("a$0c")),
            ("abc", "b", "\\$", "q", Some("a\\$c")),
            ("a b", " ", "_", "qx", Some("a_b")),
            ("a", " ", "_", "qx", Some("a")),
            ("a\u{2003}b", "\u{2003}", "_", "qx", Some("a_b")),
            ("A", "a", "$0", "qixmsq", Some("$0")),
            ("é猫é", "(é)", "🐈$1", "", Some("🐈é猫🐈é")),
        ] {
            let regex = compile_pattern(pattern, Some(flags)).unwrap().unwrap();
            assert_eq!(
                replace_xpath_with_cancel(&regex, text, replacement, &|| false)
                    .as_ref()
                    .map(Option::as_deref),
                Ok(expected),
                "text={text:?}, pattern={pattern:?}, replacement={replacement:?}, flags={flags:?}"
            );
        }
        assert!(compile_pattern("a", Some("qz")).unwrap().is_none());
        let quoted_space = compile_pattern(" ", Some("qx")).unwrap().unwrap();
        assert!(quoted_space.is_match("a b"));
        assert!(!quoted_space.is_match("ab"));
        // Numeric reference parsing is bounded even when N cannot fit usize.
        let digits = "9".repeat(1_024);
        let regex = compile_pattern("(a)", None).unwrap().unwrap();
        assert_eq!(
            replace_xpath_with_cancel(&regex, "a", &format!("${digits}"), &|| false),
            Ok(Some(digits[1..].to_owned()))
        );
    }

    #[test]
    fn static_pattern_caches_success_and_expression_error_lazily() {
        for pattern in ["a", "["] {
            let expression = Expression::Literal(Literal::from(pattern));
            let cache = static_pattern_if_exists(&expression, None).unwrap();
            assert!(cache.compiled.get().is_none());
            let first = cache.get::<std::convert::Infallible>().unwrap();
            let second = cache.get::<std::convert::Infallible>().unwrap();
            assert!(cache.compiled.get().is_some());
            if pattern == "a" {
                assert!(std::ptr::eq(first.unwrap(), second.unwrap()));
            } else {
                assert!(first.is_none());
                assert!(second.is_none());
                assert!(matches!(cache.compiled.get(), Some(Ok(None))));
            }
        }
    }

    #[test]
    fn static_and_dynamic_pattern_limits_are_fatal() {
        // The repetition expands past the configured compiled-program limit.
        // It does not require a large input allocation or unbounded matching.
        let pattern = "a{200001}";
        assert!(matches!(
            compile_pattern(pattern, None),
            Err(RegexError::ResourceLimit { .. })
        ));
        let expression = Expression::Literal(Literal::from(pattern));
        let cache = static_pattern_if_exists(&expression, None).unwrap();
        assert!(cache.compiled.get().is_none());
        for _ in 0..2 {
            assert!(matches!(
                cache.get::<std::convert::Infallible>(),
                Err(ExpressionEvaluationError::ResourceLimit { .. })
            ));
            assert!(matches!(
                cache.compiled.get(),
                Some(Err(RegexError::ResourceLimit { .. }))
            ));
        }
    }

    #[test]
    fn uuid() {
        let mut buffer = String::default();
        generate_uuid(&mut buffer);
        assert!(
            regex::Regex::new(
                "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
            )
            .unwrap()
            .is_match(&buffer),
            "{buffer} is not a valid UUID"
        );
    }
}
