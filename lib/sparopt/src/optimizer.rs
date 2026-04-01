use crate::algebra::{
    Expression, GraphPattern, JoinAlgorithm, LeftJoinAlgorithm, MinusAlgorithm, OrderExpression,
};
use crate::type_inference::{
    VariableType, VariableTypes, infer_expression_type, infer_graph_pattern_types,
};
use oxrdf::{NamedNode, Variable};
use spargebra::algebra::PropertyPathExpression;
use spargebra::term::{GroundTermPattern, NamedNodePattern};
use std::cmp::{max, min};
use std::collections::HashSet;
use std::env;
use std::sync::OnceLock;

// Heuristics for transitive path cardinality estimation.
//
// Open Kleene paths (`p*`, `p+`) are often much more expensive than plain triple patterns,
// even with one bound endpoint, because they may trigger broad graph traversal before joins
// can reduce intermediate cardinality.
const DEFAULT_OPEN_TRANSITIVE_PATH_FACTOR: usize = 100_000;
const DEFAULT_OPEN_TRANSITIVE_PATH_MIN_COST: usize = 1_000_000;
const DEFAULT_OPEN_TRANSITIVE_PATH_FULL_SCAN_COST: usize = 1_000_000_000;
const DEFAULT_TRANSITIVE_LATERAL_MAX_LEFT_SIZE: usize = 16;
const DEFAULT_SUBCLASS_TRANSITIVE_LATERAL_MAX_LEFT_SIZE: usize = 10_000;
const DEFAULT_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE: usize = 10_000;
const DEFAULT_FILTER_EXISTS_REORDER_MIN_CORRELATED_VARS: usize = 5;
const DEFAULT_JOIN_UNION_DISTRIBUTION_MAX_FACTOR_COST: usize = 1_000_000;
const DEFAULT_JOIN_UNION_DISTRIBUTION_MAX_BRANCHES: usize = 8;
const RDFS_SUBCLASS_OF_IRI: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

pub struct Optimizer;

fn open_transitive_path_factor() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_OPEN_TRANSITIVE_PATH_FACTOR")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_OPEN_TRANSITIVE_PATH_FACTOR)
    })
}

fn open_transitive_path_min_cost() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_OPEN_TRANSITIVE_PATH_MIN_COST")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_OPEN_TRANSITIVE_PATH_MIN_COST)
    })
}

fn open_transitive_path_full_scan_cost() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_OPEN_TRANSITIVE_PATH_FULL_SCAN_COST")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_OPEN_TRANSITIVE_PATH_FULL_SCAN_COST)
    })
}

fn transitive_lateral_max_left_size() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_TRANSITIVE_LATERAL_MAX_LEFT_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_TRANSITIVE_LATERAL_MAX_LEFT_SIZE)
    })
}

fn subclass_transitive_lateral_max_left_size() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_SUBCLASS_TRANSITIVE_LATERAL_MAX_LEFT_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SUBCLASS_TRANSITIVE_LATERAL_MAX_LEFT_SIZE)
    })
}

fn subclass_for_loop_max_left_size() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE)
    })
}

fn filter_exists_reorder_min_correlated_vars() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_FILTER_EXISTS_REORDER_MIN_CORRELATED_VARS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_FILTER_EXISTS_REORDER_MIN_CORRELATED_VARS)
    })
}

fn join_union_distribution_max_factor_cost() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_JOIN_UNION_DISTRIBUTION_MAX_FACTOR_COST")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_JOIN_UNION_DISTRIBUTION_MAX_FACTOR_COST)
    })
}

fn join_union_distribution_max_branches() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_JOIN_UNION_DISTRIBUTION_MAX_BRANCHES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_JOIN_UNION_DISTRIBUTION_MAX_BRANCHES)
    })
}

impl Optimizer {
    pub fn optimize_graph_pattern(pattern: GraphPattern) -> GraphPattern {
        Self::optimize_graph_pattern_with_input_types(pattern, &VariableTypes::default())
    }

    fn optimize_graph_pattern_with_input_types(
        pattern: GraphPattern,
        input_types: &VariableTypes,
    ) -> GraphPattern {
        let pattern = Self::normalize_pattern(pattern, input_types);
        let pattern = Self::reorder_joins(pattern, input_types);
        Self::push_filters(pattern, Vec::new(), input_types)
    }

    /// Normalize the pattern, discarding any join ordering information
    fn normalize_pattern(pattern: GraphPattern, input_types: &VariableTypes) -> GraphPattern {
        match pattern {
            GraphPattern::QuadPattern {
                subject,
                predicate,
                object,
                graph_name,
            } => GraphPattern::QuadPattern {
                subject,
                predicate,
                object,
                graph_name,
            },
            GraphPattern::Path {
                subject,
                path,
                object,
                graph_name,
            } => GraphPattern::Path {
                subject,
                path,
                object,
                graph_name,
            },
            GraphPattern::Graph { graph_name } => GraphPattern::Graph { graph_name },
            GraphPattern::Join {
                left,
                right,
                algorithm,
            } => GraphPattern::join(
                Self::normalize_pattern(*left, input_types),
                Self::normalize_pattern(*right, input_types),
                algorithm,
            ),
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
                algorithm,
            } => {
                let left = Self::normalize_pattern(*left, input_types);
                let right = Self::normalize_pattern(*right, input_types);
                let mut inner_types = infer_graph_pattern_types(&left, input_types.clone());
                inner_types.intersect_with(infer_graph_pattern_types(&right, input_types.clone()));
                GraphPattern::left_join(
                    left,
                    right,
                    Self::normalize_expression(expression, &inner_types),
                    algorithm,
                )
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                let left = Self::normalize_pattern(*left, input_types);
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let right = Self::normalize_pattern(*right, &left_types);
                GraphPattern::lateral(left, right)
            }
            GraphPattern::Filter { inner, expression } => {
                let inner = Self::normalize_pattern(*inner, input_types);
                let inner_types = infer_graph_pattern_types(&inner, input_types.clone());
                let expression = Self::normalize_expression(expression, &inner_types);
                let expression_type = infer_expression_type(&expression, &inner_types);
                if expression_type == VariableType::UNDEF {
                    GraphPattern::empty()
                } else {
                    GraphPattern::filter(inner, expression)
                }
            }
            GraphPattern::Union { inner } => {
                let flattened = GraphPattern::union_all(
                    inner
                        .into_iter()
                        .map(|e| Self::normalize_pattern(e, input_types))
                        .collect::<Vec<_>>(),
                );
                if let GraphPattern::Union { inner } = flattened {
                    factor_common_join_factors_from_union(inner)
                } else {
                    flattened
                }
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let inner = Self::normalize_pattern(*inner, input_types);
                let inner_types = infer_graph_pattern_types(&inner, input_types.clone());
                let expression = Self::normalize_expression(expression, &inner_types);
                let expression_type = infer_expression_type(&expression, &inner_types);
                if expression_type == VariableType::UNDEF {
                    // TODO: valid?
                    inner
                } else {
                    GraphPattern::extend(inner, variable, expression)
                }
            }
            GraphPattern::Minus {
                left,
                right,
                algorithm,
            } => GraphPattern::minus(
                Self::normalize_pattern(*left, input_types),
                Self::normalize_pattern(*right, input_types),
                algorithm,
            ),
            GraphPattern::Values {
                variables,
                bindings,
            } => GraphPattern::values(variables, bindings),
            GraphPattern::OrderBy { inner, expression } => {
                let inner = Self::normalize_pattern(*inner, input_types);
                let inner_types = infer_graph_pattern_types(&inner, input_types.clone());
                GraphPattern::order_by(
                    inner,
                    expression
                        .into_iter()
                        .map(|e| match e {
                            OrderExpression::Asc(e) => {
                                OrderExpression::Asc(Self::normalize_expression(e, &inner_types))
                            }
                            OrderExpression::Desc(e) => {
                                OrderExpression::Desc(Self::normalize_expression(e, &inner_types))
                            }
                        })
                        .collect(),
                )
            }
            GraphPattern::Project { inner, variables } => {
                GraphPattern::project(Self::normalize_pattern(*inner, input_types), variables)
            }
            GraphPattern::Distinct { inner } => {
                GraphPattern::distinct(Self::normalize_pattern(*inner, input_types))
            }
            GraphPattern::Reduced { inner } => {
                GraphPattern::reduced(Self::normalize_pattern(*inner, input_types))
            }
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::slice(Self::normalize_pattern(*inner, input_types), start, length),
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => {
                // TODO: min, max and sample don't care about DISTINCT
                GraphPattern::group(
                    Self::normalize_pattern(*inner, input_types),
                    variables,
                    aggregates,
                )
            }
            GraphPattern::Service { .. } => {
                // We leave this problem to the remote SPARQL endpoint
                pattern
            }
        }
    }

    fn normalize_expression(expression: Expression, types: &VariableTypes) -> Expression {
        match expression {
            Expression::NamedNode(node) => node.into(),
            Expression::Literal(literal) => literal.into(),
            Expression::Variable(variable) => variable.into(),
            Expression::Or(inner) => Expression::or_all(
                inner
                    .into_iter()
                    .map(|e| Self::normalize_expression(e, types)),
            ),
            Expression::And(inner) => Expression::and_all(
                inner
                    .into_iter()
                    .map(|e| Self::normalize_expression(e, types)),
            ),
            Expression::Equal(left, right) => {
                let left = Self::normalize_expression(*left, types);
                let left_types = infer_expression_type(&left, types);
                let right = Self::normalize_expression(*right, types);
                let right_types = infer_expression_type(&right, types);
                #[allow(unused_mut, clippy::allow_attributes)]
                let mut must_use_equal = left_types.literal && right_types.literal;
                #[cfg(feature = "sparql-12")]
                {
                    must_use_equal = must_use_equal || left_types.triple && right_types.triple;
                }
                if must_use_equal {
                    Expression::equal(left, right)
                } else {
                    Expression::same_term(left, right)
                }
            }
            Expression::SameTerm(left, right) => Expression::same_term(
                Self::normalize_expression(*left, types),
                Self::normalize_expression(*right, types),
            ),
            Expression::Greater(left, right) => Expression::greater(
                Self::normalize_expression(*left, types),
                Self::normalize_expression(*right, types),
            ),
            Expression::GreaterOrEqual(left, right) => Expression::greater_or_equal(
                Self::normalize_expression(*left, types),
                Self::normalize_expression(*right, types),
            ),
            Expression::Less(left, right) => Expression::less(
                Self::normalize_expression(*left, types),
                Self::normalize_expression(*right, types),
            ),
            Expression::LessOrEqual(left, right) => Expression::less_or_equal(
                Self::normalize_expression(*left, types),
                Self::normalize_expression(*right, types),
            ),
            Expression::Add(left, right) => {
                Self::normalize_expression(*left, types) + Self::normalize_expression(*right, types)
            }
            Expression::Subtract(left, right) => {
                Self::normalize_expression(*left, types) - Self::normalize_expression(*right, types)
            }
            Expression::Multiply(left, right) => {
                Self::normalize_expression(*left, types) * Self::normalize_expression(*right, types)
            }
            Expression::Divide(left, right) => {
                Self::normalize_expression(*left, types) / Self::normalize_expression(*right, types)
            }
            Expression::UnaryPlus(inner) => {
                Expression::unary_plus(Self::normalize_expression(*inner, types))
            }
            Expression::UnaryMinus(inner) => -Self::normalize_expression(*inner, types),
            Expression::Not(inner) => !Self::normalize_expression(*inner, types),
            Expression::Exists(inner) => Expression::exists(Self::normalize_pattern(*inner, types)),
            Expression::Bound(variable) => {
                let t = types.get(&variable);
                if !t.undef {
                    true.into()
                } else if t == VariableType::UNDEF {
                    false.into()
                } else {
                    Expression::Bound(variable)
                }
            }
            Expression::If(cond, then, els) => Expression::if_cond(
                Self::normalize_expression(*cond, types),
                Self::normalize_expression(*then, types),
                Self::normalize_expression(*els, types),
            ),
            Expression::Coalesce(inners) => Expression::coalesce(
                inners
                    .into_iter()
                    .map(|e| Self::normalize_expression(e, types))
                    .collect(),
            ),
            Expression::FunctionCall(name, args) => Expression::call(
                name,
                args.into_iter()
                    .map(|e| Self::normalize_expression(e, types))
                    .collect(),
            ),
        }
    }

    fn push_filters(
        pattern: GraphPattern,
        mut filters: Vec<Expression>,
        input_types: &VariableTypes,
    ) -> GraphPattern {
        match pattern {
            GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. }
            | GraphPattern::Values { .. } => {
                GraphPattern::filter(pattern, Expression::and_all(filters))
            }
            GraphPattern::Join {
                left,
                right,
                algorithm,
            } => {
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let right_types = infer_graph_pattern_types(&right, input_types.clone());
                let mut left_filters = Vec::new();
                let mut right_filters = Vec::new();
                let mut final_filters = Vec::new();
                for filter in filters {
                    let push_left = are_all_expression_variables_bound(&filter, &left_types);
                    let push_right = are_all_expression_variables_bound(&filter, &right_types);
                    if push_left {
                        if push_right {
                            left_filters.push(filter.clone());
                            right_filters.push(filter);
                        } else {
                            left_filters.push(filter);
                        }
                    } else if push_right {
                        right_filters.push(filter);
                    } else {
                        final_filters.push(filter);
                    }
                }
                GraphPattern::filter(
                    GraphPattern::join(
                        Self::push_filters(*left, left_filters, input_types),
                        Self::push_filters(*right, right_filters, input_types),
                        algorithm,
                    ),
                    Expression::and_all(final_filters),
                )
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let mut left_filters = Vec::new();
                let mut right_filters = Vec::new();
                for filter in filters {
                    let push_left = are_all_expression_variables_bound(&filter, &left_types);
                    if push_left {
                        left_filters.push(filter);
                    } else {
                        right_filters.push(filter);
                    }
                }
                let left = Self::push_filters(*left, left_filters, input_types);
                let right = Self::push_filters(*right, right_filters, &left_types);
                if let GraphPattern::Filter {
                    inner: inner_right,
                    expression,
                } = right
                {
                    // We prefer to have filter out of the lateral rather than inside the right part
                    GraphPattern::filter(GraphPattern::lateral(left, *inner_right), expression)
                } else {
                    GraphPattern::lateral(left, right)
                }
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
                algorithm,
            } => {
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let right_types = infer_graph_pattern_types(&right, input_types.clone());
                let mut left_filters = Vec::new();
                let mut right_filters = Vec::new();
                let mut final_filters = Vec::new();
                for filter in filters {
                    let push_left = are_all_expression_variables_bound(&filter, &left_types);
                    if push_left {
                        left_filters.push(filter);
                    } else {
                        final_filters.push(filter);
                    }
                }
                let expression = if expression.effective_boolean_value().is_none()
                    && (are_all_expression_variables_bound(&expression, &right_types)
                        || are_no_expression_variables_bound(&expression, &left_types))
                {
                    right_filters.push(expression);
                    true.into()
                } else {
                    expression
                };
                GraphPattern::filter(
                    GraphPattern::left_join(
                        Self::push_filters(*left, left_filters, input_types),
                        Self::push_filters(*right, right_filters, input_types),
                        expression,
                        algorithm,
                    ),
                    Expression::and_all(final_filters),
                )
            }
            GraphPattern::Minus {
                left,
                right,
                algorithm,
            } => GraphPattern::minus(
                Self::push_filters(*left, filters, input_types),
                Self::push_filters(*right, Vec::new(), input_types),
                algorithm,
            ),
            GraphPattern::Extend {
                inner,
                expression,
                variable,
            } => {
                // TODO: handle the case where the filter overrides an expression variable (should not happen in SPARQL but allowed in the algebra)
                let mut inner_filters = Vec::new();
                let mut final_filters = Vec::new();
                for filter in filters {
                    let extend_variable_used =
                        filter.used_variables().into_iter().any(|v| *v == variable);
                    if extend_variable_used {
                        final_filters.push(filter);
                    } else {
                        inner_filters.push(filter);
                    }
                }
                GraphPattern::filter(
                    GraphPattern::extend(
                        Self::push_filters(*inner, inner_filters, input_types),
                        variable,
                        expression,
                    ),
                    Expression::and_all(final_filters),
                )
            }
            GraphPattern::Filter { inner, expression } => {
                if let Expression::And(expressions) = expression {
                    filters.extend(expressions)
                } else {
                    filters.push(expression)
                }
                Self::push_filters(*inner, filters, input_types)
            }
            GraphPattern::Union { inner } => GraphPattern::union_all(
                inner
                    .into_iter()
                    .map(|c| Self::push_filters(c, filters.clone(), input_types)),
            ),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::filter(
                GraphPattern::slice(
                    Self::push_filters(*inner, Vec::new(), input_types),
                    start,
                    length,
                ),
                Expression::and_all(filters),
            ),
            GraphPattern::Distinct { inner } => {
                GraphPattern::distinct(Self::push_filters(*inner, filters, input_types))
            }
            GraphPattern::Reduced { inner } => {
                GraphPattern::reduced(Self::push_filters(*inner, filters, input_types))
            }
            GraphPattern::Project { inner, variables } => {
                // Projection is a scope barrier: pushing filters below it can expose
                // projected-away variables to EXISTS/NOT EXISTS correlation.
                GraphPattern::filter(
                    GraphPattern::project(
                        Self::push_filters(*inner, Vec::new(), input_types),
                        variables,
                    ),
                    Expression::and_all(filters),
                )
            }
            GraphPattern::OrderBy { inner, expression } => {
                GraphPattern::order_by(Self::push_filters(*inner, filters, input_types), expression)
            }
            GraphPattern::Service { .. } => {
                // TODO: we can be smart and push some filters
                // But we need to check the behavior of SILENT that can transform no results into a singleton
                GraphPattern::filter(pattern, Expression::and_all(filters))
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::filter(
                GraphPattern::group(
                    Self::push_filters(*inner, Vec::new(), input_types),
                    variables,
                    aggregates,
                ),
                Expression::and_all(filters),
            ),
        }
    }

    fn reorder_joins(pattern: GraphPattern, input_types: &VariableTypes) -> GraphPattern {
        match pattern {
            GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Values { .. }
            | GraphPattern::Graph { .. } => pattern,
            GraphPattern::Join { left, right, .. } => {
                let left = *left;
                let right = *right;
                if let Some(rewritten) = rewrite_inner_join_with_correlated_filter_only_union_branch(
                    left.clone(),
                    right.clone(),
                    input_types,
                ) {
                    return Self::reorder_joins(rewritten, input_types);
                }
                if let Some(rewritten) = rewrite_inner_join_with_small_factor_union(
                    left.clone(),
                    right.clone(),
                    input_types,
                ) {
                    return Self::reorder_joins(rewritten, input_types);
                }
                if let Some(rewritten) = rewrite_inner_join_with_singleton_union_branch(
                    left.clone(),
                    right.clone(),
                    input_types,
                ) {
                    return Self::reorder_joins(rewritten, input_types);
                }
                if let Some(fallback) =
                    guarded_singleton_union_inner_join_fallback(left.clone(), right.clone(), input_types)
                {
                    return fallback;
                }
                // We flatten the join operation
                let mut to_reorder = Vec::new();
                let mut todo = vec![right, left];
                while let Some(e) = todo.pop() {
                    if let GraphPattern::Join { left, right, .. } = e {
                        todo.push(*right);
                        todo.push(*left);
                    } else {
                        // Ensure nested joins are reordered even when they are wrapped
                        // inside non-join operators (e.g., FILTER or UNION branches).
                        let factor = Self::reorder_joins(e, input_types);
                        if can_deduplicate_join_factor(&factor)
                            && to_reorder.iter().any(|existing| existing == &factor)
                        {
                            continue;
                        }
                        to_reorder.push(factor);
                    }
                }
                if let Some(rewritten) =
                    rewrite_flat_join_with_small_factor_union(to_reorder.clone(), input_types)
                {
                    return Self::reorder_joins(rewritten, input_types);
                }
                if to_reorder.len() > 2
                    && let Some((singleton_union_index, non_singleton_branches)) =
                        to_reorder.iter().enumerate().find_map(|(index, pattern)| {
                            split_singleton_union_branches(pattern).and_then(
                                |(singleton_branch_count, non_singleton_branches)| {
                                    if singleton_branch_count == 1 {
                                        Some((index, non_singleton_branches))
                                    } else {
                                        None
                                    }
                                },
                            )
                        })
                {
                    let mut base_factors = to_reorder.clone();
                    base_factors.remove(singleton_union_index);
                    let base_join = Self::reorder_joins(
                        join_all_factors(base_factors.clone())
                            .expect("join flattening should keep at least one factor"),
                        input_types,
                    );
                    if non_singleton_branches.is_empty() {
                        return base_join;
                    }
                    if should_avoid_singleton_union_duplication(&base_join, input_types) {
                        if base_factors.iter().any(|factor| {
                            should_avoid_singleton_union_duplication(factor, input_types)
                        }) {
                            if let Some(rewritten) =
                                rewrite_flat_join_with_guarded_singleton_union_branch(
                                    base_factors.clone(),
                                    non_singleton_branches.clone(),
                                    input_types,
                                )
                            {
                                return rewritten;
                            }
                        }
                        let mut reordered_base_factors = Vec::new();
                        flatten_inner_join_factors(base_join.clone(), &mut reordered_base_factors);
                        if let Some(rewritten) =
                            rewrite_flat_join_with_guarded_singleton_union_branch(
                                reordered_base_factors,
                                non_singleton_branches.clone(),
                                input_types,
                            )
                        {
                            return rewritten;
                        }
                    }
                    let mut branch_factors = base_factors;
                    branch_factors.push(GraphPattern::union_all(non_singleton_branches));
                    return GraphPattern::union_all([
                        base_join,
                        Self::reorder_joins(
                            join_all_factors(branch_factors)
                                .expect("join flattening should keep at least one factor"),
                            input_types,
                        ),
                    ]);
                }

                // We do first type inference
                let to_reorder_types = to_reorder
                    .iter()
                    .map(|p| infer_graph_pattern_types(p, input_types.clone()))
                    .collect::<Vec<_>>();

                // We do greedy join reordering
                let mut output_cartesian_product_joins = Vec::new();
                let mut not_yet_reordered_ids = vec![true; to_reorder.len()];
                // We look for the next connected component to reorder and pick the smallest element
                while let Some(next_entry_id) = not_yet_reordered_ids
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| **v)
                    .map(|(i, _)| i)
                    .min_by_key(|i| {
                        (
                            estimate_graph_pattern_size(&to_reorder[*i], input_types),
                            graph_pattern_reordering_penalty(&to_reorder[*i], input_types),
                        )
                    })
                {
                    not_yet_reordered_ids[next_entry_id] = false; // It's now done
                    let mut output = to_reorder[next_entry_id].clone();
                    let mut output_types = to_reorder_types[next_entry_id].clone();
                    // We look for an other child to join with that does not blow up the join cost
                    while let Some(next_id) = not_yet_reordered_ids
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| **v)
                        .map(|(i, _)| i)
                        .filter(|i| {
                            has_common_variables(&output_types, &to_reorder_types[*i], input_types)
                        })
                        .min_by_key(|i| {
                            #[cfg(feature = "sep-0006")]
                            let join_cost = {
                                let output_size =
                                    estimate_for_loop_entry_size(&output, input_types);
                                if is_fit_for_for_loop_join(
                                    &to_reorder[*i],
                                    input_types,
                                    &output_types,
                                    output_size,
                                ) {
                                    estimate_lateral_cost(
                                        &output,
                                        &output_types,
                                        &to_reorder[*i],
                                        input_types,
                                    )
                                } else {
                                    estimate_join_cost(
                                        &output,
                                        &to_reorder[*i],
                                        &JoinAlgorithm::HashBuildLeftProbeRight {
                                            keys: join_key_variables(
                                                &output_types,
                                                &to_reorder_types[*i],
                                                input_types,
                                            ),
                                        },
                                        input_types,
                                    )
                                }
                            };
                            #[cfg(not(feature = "sep-0006"))]
                            let join_cost = estimate_join_cost(
                                &output,
                                &to_reorder[*i],
                                &JoinAlgorithm::HashBuildLeftProbeRight {
                                    keys: join_key_variables(
                                        &output_types,
                                        &to_reorder_types[*i],
                                        input_types,
                                    ),
                                },
                                input_types,
                            );
                            (
                                join_cost,
                                graph_pattern_reordering_penalty(&to_reorder[*i], &output_types),
                            )
                        })
                    {
                        not_yet_reordered_ids[next_id] = false; // It's now done
                        let next = to_reorder[next_id].clone();
                        #[cfg(feature = "sep-0006")]
                        {
                            let output_size = estimate_for_loop_entry_size(&output, input_types);
                            output = if is_fit_for_for_loop_join(
                                &next,
                                input_types,
                                &output_types,
                                output_size,
                            ) {
                                GraphPattern::lateral(
                                    output,
                                    Self::reorder_joins(next, &output_types),
                                )
                            } else {
                                GraphPattern::join(
                                    output,
                                    next,
                                    JoinAlgorithm::HashBuildLeftProbeRight {
                                        keys: join_key_variables(
                                            &output_types,
                                            &to_reorder_types[next_id],
                                            input_types,
                                        ),
                                    },
                                )
                            };
                        }
                        #[cfg(not(feature = "sep-0006"))]
                        {
                            output = GraphPattern::join(
                                output,
                                next,
                                JoinAlgorithm::HashBuildLeftProbeRight {
                                    keys: join_key_variables(
                                        &output_types,
                                        &to_reorder_types[next_id],
                                        input_types,
                                    ),
                                },
                            );
                        }
                        output_types.intersect_with(to_reorder_types[next_id].clone());
                    }
                    output_cartesian_product_joins.push(output);
                }
                output_cartesian_product_joins
                    .into_iter()
                    .reduce(|left, right| {
                        let keys = join_key_variables(
                            &infer_graph_pattern_types(&left, input_types.clone()),
                            &infer_graph_pattern_types(&right, input_types.clone()),
                            input_types,
                        );
                        if estimate_graph_pattern_size(&left, input_types)
                            <= estimate_graph_pattern_size(&right, input_types)
                        {
                            GraphPattern::join(
                                left,
                                right,
                                JoinAlgorithm::HashBuildLeftProbeRight { keys },
                            )
                        } else {
                            GraphPattern::join(
                                right,
                                left,
                                JoinAlgorithm::HashBuildLeftProbeRight { keys },
                            )
                        }
                    })
                    .unwrap()
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                GraphPattern::lateral(
                    Self::reorder_joins(*left, input_types),
                    Self::reorder_joins(*right, &left_types),
                )
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
                ..
            } => {
                let left = Self::reorder_joins(*left, input_types);
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let right = Self::reorder_joins(*right, input_types);
                let right_types = infer_graph_pattern_types(&right, input_types.clone());
                if expression.effective_boolean_value() == Some(true) {
                    if let Some((singleton_branch_count, non_singleton_branches)) =
                        split_singleton_union_branches(&right)
                    {
                        if singleton_branch_count == 1 {
                            if should_avoid_singleton_union_duplication(&left, input_types) {
                                return GraphPattern::left_join(
                                    left,
                                    right,
                                    expression,
                                    LeftJoinAlgorithm::HashBuildRightProbeLeft {
                                        keys: join_key_variables(
                                            &left_types,
                                            &right_types,
                                            input_types,
                                        ),
                                    },
                                );
                            }
                            if non_singleton_branches.is_empty() {
                                return left;
                            }
                            let right_without_singleton =
                                GraphPattern::union_all(non_singleton_branches);
                            let right_types = infer_graph_pattern_types(
                                &right_without_singleton,
                                input_types.clone(),
                            );
                            return GraphPattern::union_all([
                                left.clone(),
                                GraphPattern::join(
                                    left,
                                    right_without_singleton,
                                    JoinAlgorithm::HashBuildLeftProbeRight {
                                        keys: join_key_variables(
                                            &left_types,
                                            &right_types,
                                            input_types,
                                        ),
                                    },
                                ),
                            ]);
                        }
                    }
                }
                #[cfg(feature = "sep-0006")]
                {
                    let left_size = estimate_graph_pattern_size(&left, input_types);
                    if is_fit_for_for_loop_join(&right, input_types, &left_types, left_size)
                        && has_common_variables(&left_types, &right_types, input_types)
                    {
                        return GraphPattern::lateral(
                            left,
                            GraphPattern::left_join(
                                GraphPattern::empty_singleton(),
                                right,
                                expression,
                                LeftJoinAlgorithm::HashBuildRightProbeLeft { keys: Vec::new() },
                            ),
                        );
                    }
                }
                GraphPattern::left_join(
                    left,
                    right,
                    expression,
                    LeftJoinAlgorithm::HashBuildRightProbeLeft {
                        keys: join_key_variables(&left_types, &right_types, input_types),
                    },
                )
            }
            GraphPattern::Minus { left, right, .. } => {
                let left = Self::reorder_joins(*left, input_types);
                let left_types = infer_graph_pattern_types(&left, input_types.clone());
                let right = Self::reorder_joins(*right, input_types);
                let right_types = infer_graph_pattern_types(&right, input_types.clone());
                GraphPattern::minus(
                    left,
                    right,
                    MinusAlgorithm::HashBuildRightProbeLeft {
                        keys: join_key_variables(&left_types, &right_types, input_types),
                    },
                )
            }
            GraphPattern::Extend {
                inner,
                expression,
                variable,
            } => GraphPattern::extend(Self::reorder_joins(*inner, input_types), variable, expression),
            GraphPattern::Filter { inner, expression } => {
                let inner = Self::reorder_joins(*inner, input_types);
                let inner_types = infer_graph_pattern_types(&inner, input_types.clone());
                GraphPattern::filter(
                    inner,
                    Self::reorder_filter_exists_expressions(expression, &inner_types),
                )
            }
            GraphPattern::Union { inner } => GraphPattern::union_all(
                inner
                    .into_iter()
                    .map(|c| Self::reorder_joins(c, input_types)),
            ),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::slice(Self::reorder_joins(*inner, input_types), start, length),
            GraphPattern::Distinct { inner } => {
                GraphPattern::distinct(Self::reorder_joins(*inner, input_types))
            }
            GraphPattern::Reduced { inner } => {
                GraphPattern::reduced(Self::reorder_joins(*inner, input_types))
            }
            GraphPattern::Project { inner, variables } => {
                GraphPattern::project(Self::reorder_joins(*inner, input_types), variables)
            }
            GraphPattern::OrderBy { inner, expression } => {
                GraphPattern::order_by(Self::reorder_joins(*inner, input_types), expression)
            }
            GraphPattern::Service { .. } => {
                // We don't do join reordering inside of SERVICE calls, we don't know about cardinalities
                pattern
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::group(
                Self::reorder_joins(*inner, input_types),
                variables,
                aggregates,
            ),
        }
    }

    fn reorder_filter_exists_expressions(
        expression: Expression,
        input_types: &VariableTypes,
    ) -> Expression {
        match expression {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::Bound(_) => expression,
            Expression::Or(inner) => Expression::or_all(
                inner
                    .into_iter()
                    .map(|e| Self::reorder_filter_exists_expressions(e, input_types))
                    .collect::<Vec<_>>(),
            ),
            Expression::And(inner) => Expression::and_all(
                inner
                    .into_iter()
                    .map(|e| Self::reorder_filter_exists_expressions(e, input_types))
                    .collect::<Vec<_>>(),
            ),
            Expression::Equal(left, right) => Expression::equal(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::SameTerm(left, right) => Expression::same_term(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::Greater(left, right) => Expression::greater(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::GreaterOrEqual(left, right) => Expression::greater_or_equal(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::Less(left, right) => Expression::less(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::LessOrEqual(left, right) => Expression::less_or_equal(
                Self::reorder_filter_exists_expressions(*left, input_types),
                Self::reorder_filter_exists_expressions(*right, input_types),
            ),
            Expression::Add(left, right) => {
                Self::reorder_filter_exists_expressions(*left, input_types)
                    + Self::reorder_filter_exists_expressions(*right, input_types)
            }
            Expression::Subtract(left, right) => {
                Self::reorder_filter_exists_expressions(*left, input_types)
                    - Self::reorder_filter_exists_expressions(*right, input_types)
            }
            Expression::Multiply(left, right) => {
                Self::reorder_filter_exists_expressions(*left, input_types)
                    * Self::reorder_filter_exists_expressions(*right, input_types)
            }
            Expression::Divide(left, right) => {
                Self::reorder_filter_exists_expressions(*left, input_types)
                    / Self::reorder_filter_exists_expressions(*right, input_types)
            }
            Expression::UnaryPlus(inner) => {
                Expression::unary_plus(Self::reorder_filter_exists_expressions(*inner, input_types))
            }
            Expression::UnaryMinus(inner) => {
                -Self::reorder_filter_exists_expressions(*inner, input_types)
            }
            Expression::Not(inner) => !Self::reorder_filter_exists_expressions(*inner, input_types),
            Expression::If(cond, then, els) => Expression::if_cond(
                Self::reorder_filter_exists_expressions(*cond, input_types),
                Self::reorder_filter_exists_expressions(*then, input_types),
                Self::reorder_filter_exists_expressions(*els, input_types),
            ),
            Expression::Coalesce(inner) => Expression::coalesce(
                inner
                    .into_iter()
                    .map(|e| Self::reorder_filter_exists_expressions(e, input_types))
                    .collect::<Vec<_>>(),
            ),
            Expression::Exists(inner) => {
                if should_reorder_highly_correlated_filter_exists(&inner, input_types) {
                    Expression::exists(Self::optimize_graph_pattern_with_input_types(
                        *inner,
                        input_types,
                    ))
                } else {
                    Expression::exists(*inner)
                }
            }
            Expression::FunctionCall(name, args) => Expression::call(
                name,
                args.into_iter()
                    .map(|e| Self::reorder_filter_exists_expressions(e, input_types))
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

fn should_reorder_highly_correlated_filter_exists(
    inner: &GraphPattern,
    input_types: &VariableTypes,
) -> bool {
    count_outer_bound_variables_used_in_pattern(inner, input_types)
        >= filter_exists_reorder_min_correlated_vars()
        && graph_pattern_contains_kleene_subclass_path(inner)
        && !matches!(
            inner,
            GraphPattern::Union { .. } | GraphPattern::Group { .. } | GraphPattern::Service { .. }
        )
}

fn count_outer_bound_variables_used_in_pattern(
    pattern: &GraphPattern,
    input_types: &VariableTypes,
) -> usize {
    let mut variables = HashSet::new();
    pattern.lookup_used_variables(&mut |variable| {
        variables.insert(variable.clone());
    });
    variables
        .into_iter()
        .filter(|variable| input_types.get(variable) != VariableType::UNDEF)
        .count()
}

fn graph_pattern_contains_kleene_subclass_path(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Path { path, .. } => {
            rdfs_subclass_of_kleene_transitive_path_bound_endpoint(path).is_some()
        }
        GraphPattern::Join { left, right, .. }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Minus { left, right, .. } => {
            graph_pattern_contains_kleene_subclass_path(left)
                || graph_pattern_contains_kleene_subclass_path(right)
        }
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { left, right } => {
            graph_pattern_contains_kleene_subclass_path(left)
                || graph_pattern_contains_kleene_subclass_path(right)
        }
        GraphPattern::Filter { inner, expression } => {
            graph_pattern_contains_kleene_subclass_path(inner)
                || expression_contains_kleene_subclass_path(expression)
        }
        GraphPattern::Extend {
            inner, expression, ..
        } => {
            graph_pattern_contains_kleene_subclass_path(inner)
                || expression_contains_kleene_subclass_path(expression)
        }
        GraphPattern::OrderBy { inner, expression } => {
            graph_pattern_contains_kleene_subclass_path(inner)
                || expression
                    .iter()
                    .any(order_expression_contains_kleene_subclass_path)
        }
        GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. }
        | GraphPattern::Service { inner, .. } => {
            graph_pattern_contains_kleene_subclass_path(inner)
        }
        GraphPattern::Union { inner } => inner
            .iter()
            .any(graph_pattern_contains_kleene_subclass_path),
        GraphPattern::QuadPattern { .. }
        | GraphPattern::Values { .. }
        | GraphPattern::Graph { .. } => false,
    }
}

fn order_expression_contains_kleene_subclass_path(expression: &OrderExpression) -> bool {
    match expression {
        OrderExpression::Asc(expression) | OrderExpression::Desc(expression) => {
            expression_contains_kleene_subclass_path(expression)
        }
    }
}

fn expression_contains_kleene_subclass_path(expression: &Expression) -> bool {
    match expression {
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => false,
        Expression::Or(inner) | Expression::And(inner) | Expression::Coalesce(inner) => {
            inner.iter().any(expression_contains_kleene_subclass_path)
        }
        Expression::Equal(left, right)
        | Expression::SameTerm(left, right)
        | Expression::Greater(left, right)
        | Expression::GreaterOrEqual(left, right)
        | Expression::Less(left, right)
        | Expression::LessOrEqual(left, right)
        | Expression::Add(left, right)
        | Expression::Subtract(left, right)
        | Expression::Multiply(left, right)
        | Expression::Divide(left, right) => {
            expression_contains_kleene_subclass_path(left)
                || expression_contains_kleene_subclass_path(right)
        }
        Expression::UnaryPlus(inner)
        | Expression::UnaryMinus(inner)
        | Expression::Not(inner) => expression_contains_kleene_subclass_path(inner),
        Expression::Exists(inner) => graph_pattern_contains_kleene_subclass_path(inner),
        Expression::If(condition, then, els) => {
            expression_contains_kleene_subclass_path(condition)
                || expression_contains_kleene_subclass_path(then)
                || expression_contains_kleene_subclass_path(els)
        }
        Expression::FunctionCall(_, arguments) => {
            arguments.iter().any(expression_contains_kleene_subclass_path)
        }
    }
}

fn is_fit_for_for_loop_join(
    pattern: &GraphPattern,
    global_input_types: &VariableTypes,
    entry_types: &VariableTypes,
    entry_estimated_size: usize,
) -> bool {
    // TODO: think more about it
    match pattern {
        GraphPattern::Values { .. } | GraphPattern::Graph { .. } => true,
        GraphPattern::QuadPattern { predicate, .. } => {
            if is_rdfs_subclass_of_predicate(predicate)
                && entry_estimated_size > subclass_for_loop_max_left_size()
            {
                return false;
            }
            true
        }
        GraphPattern::Path {
            subject,
            path,
            object,
            ..
        } => {
            if !contains_kleene_transitive_path(path) {
                return true;
            }
            let entry_start_bound = is_term_pattern_bound(subject, entry_types);
            let entry_end_bound = is_term_pattern_bound(object, entry_types);
            if entry_estimated_size
                > transitive_lateral_max_left_size_for_path(
                    path,
                    entry_start_bound,
                    entry_end_bound,
                )
            {
                return false;
            }
            entry_start_bound || entry_end_bound
        }
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { left, right } => {
            is_fit_for_for_loop_join(left, global_input_types, entry_types, entry_estimated_size)
                && is_fit_for_for_loop_join(
                    right,
                    global_input_types,
                    entry_types,
                    entry_estimated_size,
                )
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
            ..
        } => {
            if !is_fit_for_for_loop_join(
                left,
                global_input_types,
                entry_types,
                entry_estimated_size,
            ) {
                return false;
            }

            // It is not ok to transform into for loop join if right binds a variable also bound by the entry part of the for loop join
            let mut left_types = infer_graph_pattern_types(left, global_input_types.clone());
            let right_types = infer_graph_pattern_types(right, global_input_types.clone());
            if right_types.iter().any(|(variable, t)| {
                *t != VariableType::UNDEF
                    && left_types.get(variable).undef
                    && entry_types.get(variable) != VariableType::UNDEF
            }) {
                return false;
            }

            // We don't forget the final expression
            left_types.intersect_with(right_types);
            is_expression_fit_for_for_loop_join(
                expression,
                &left_types,
                entry_types,
                entry_estimated_size,
            )
        }
        GraphPattern::Union { inner } => inner.iter().all(|i| {
            is_fit_for_for_loop_join(i, global_input_types, entry_types, entry_estimated_size)
        }),
        GraphPattern::Filter { inner, expression } => {
            is_fit_for_for_loop_join(inner, global_input_types, entry_types, entry_estimated_size)
                && is_expression_fit_for_for_loop_join(
                    expression,
                    &infer_graph_pattern_types(inner, global_input_types.clone()),
                    entry_types,
                    entry_estimated_size,
                )
        }
        GraphPattern::Extend {
            inner,
            expression,
            variable,
        } => {
            is_fit_for_for_loop_join(inner, global_input_types, entry_types, entry_estimated_size)
                && entry_types.get(variable) == VariableType::UNDEF
                && is_expression_fit_for_for_loop_join(
                    expression,
                    &infer_graph_pattern_types(inner, global_input_types.clone()),
                    entry_types,
                    entry_estimated_size,
                )
        }
        GraphPattern::Service { name, .. } => match name {
            NamedNodePattern::NamedNode(_) => true,
            NamedNodePattern::Variable(v) => !entry_types.get(v).undef,
        },
        GraphPattern::Join { .. } => {
            if !graph_pattern_contains_kleene_subclass_path(pattern) {
                return false;
            }
            let mut factors = Vec::new();
            flatten_inner_join_factors(pattern.clone(), &mut factors);
            let factor_types = factors
                .iter()
                .map(|factor| infer_graph_pattern_types(factor, global_input_types.clone()))
                .collect::<Vec<_>>();
            let mut current_types = entry_types.clone();
            let mut not_yet_ordered = vec![true; factors.len()];
            while let Some(next_id) = not_yet_ordered
                .iter()
                .enumerate()
                .filter(|(_, remaining)| **remaining)
                .map(|(idx, _)| idx)
                .filter(|idx| {
                    is_fit_for_for_loop_join(
                        &factors[*idx],
                        global_input_types,
                        &current_types,
                        entry_estimated_size,
                    )
                })
                .min_by_key(|idx| {
                    (
                        estimate_graph_pattern_size(&factors[*idx], global_input_types),
                        graph_pattern_reordering_penalty(&factors[*idx], &current_types),
                    )
                })
            {
                not_yet_ordered[next_id] = false;
                current_types.intersect_with(factor_types[next_id].clone());
            }
            not_yet_ordered.into_iter().all(|remaining| !remaining)
        }
        GraphPattern::Minus { .. }
        | GraphPattern::OrderBy { .. }
        | GraphPattern::Distinct { .. }
        | GraphPattern::Reduced { .. }
        | GraphPattern::Slice { .. }
        | GraphPattern::Project { .. }
        | GraphPattern::Group { .. } => false,
    }
}

fn contains_kleene_transitive_path(path: &PropertyPathExpression) -> bool {
    match path {
        PropertyPathExpression::NamedNode(_) | PropertyPathExpression::NegatedPropertySet(_) => {
            false
        }
        PropertyPathExpression::Reverse(inner) | PropertyPathExpression::ZeroOrOne(inner) => {
            contains_kleene_transitive_path(inner)
        }
        PropertyPathExpression::Sequence(left, right)
        | PropertyPathExpression::Alternative(left, right) => {
            contains_kleene_transitive_path(left) || contains_kleene_transitive_path(right)
        }
        PropertyPathExpression::ZeroOrMore(_) | PropertyPathExpression::OneOrMore(_) => true,
    }
}

fn transitive_lateral_max_left_size_for_path(
    path: &PropertyPathExpression,
    entry_start_bound: bool,
    entry_end_bound: bool,
) -> usize {
    if is_entry_bound_for_rdfs_subclass_of_kleene_transitive_path(
        path,
        entry_start_bound,
        entry_end_bound,
    ) {
        subclass_transitive_lateral_max_left_size()
    } else {
        transitive_lateral_max_left_size()
    }
}

fn is_entry_bound_for_rdfs_subclass_of_kleene_transitive_path(
    path: &PropertyPathExpression,
    entry_start_bound: bool,
    entry_end_bound: bool,
) -> bool {
    match rdfs_subclass_of_kleene_transitive_path_bound_endpoint(path) {
        Some(TransitivePathBoundEndpoint::Start) => entry_start_bound,
        Some(TransitivePathBoundEndpoint::End) => entry_end_bound,
        None => false,
    }
}

fn rdfs_subclass_of_kleene_transitive_path_bound_endpoint(
    path: &PropertyPathExpression,
) -> Option<TransitivePathBoundEndpoint> {
    match path {
        PropertyPathExpression::ZeroOrMore(inner) | PropertyPathExpression::OneOrMore(inner) => {
            transitive_path_base_predicate(inner).and_then(|(predicate, start_is_forward)| {
                (predicate.as_str() == RDFS_SUBCLASS_OF_IRI).then_some(if start_is_forward {
                    TransitivePathBoundEndpoint::Start
                } else {
                    TransitivePathBoundEndpoint::End
                })
            })
        }
        _ => None,
    }
}

fn transitive_path_base_predicate(path: &PropertyPathExpression) -> Option<(&NamedNode, bool)> {
    match path {
        PropertyPathExpression::NamedNode(predicate) => Some((predicate, true)),
        PropertyPathExpression::Reverse(inner) => transitive_path_base_predicate(inner)
            .map(|(predicate, start_is_forward)| (predicate, !start_is_forward)),
        _ => None,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TransitivePathBoundEndpoint {
    Start,
    End,
}

fn are_all_expression_variables_bound(
    expression: &Expression,
    variable_types: &VariableTypes,
) -> bool {
    expression
        .used_variables()
        .into_iter()
        .all(|v| !variable_types.get(v).undef)
}

fn are_no_expression_variables_bound(
    expression: &Expression,
    variable_types: &VariableTypes,
) -> bool {
    expression
        .used_variables()
        .into_iter()
        .all(|v| variable_types.get(v) == VariableType::UNDEF)
}

fn is_expression_fit_for_for_loop_join(
    expression: &Expression,
    input_types: &VariableTypes,
    entry_types: &VariableTypes,
    entry_estimated_size: usize,
) -> bool {
    match expression {
        Expression::NamedNode(_) | Expression::Literal(_) => true,
        Expression::Variable(v) | Expression::Bound(v) => {
            !input_types.get(v).undef || entry_types.get(v) == VariableType::UNDEF
        }
        Expression::Or(inner)
        | Expression::And(inner)
        | Expression::Coalesce(inner)
        | Expression::FunctionCall(_, inner) => inner.iter().all(|e| {
            is_expression_fit_for_for_loop_join(e, input_types, entry_types, entry_estimated_size)
        }),
        Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            is_expression_fit_for_for_loop_join(a, input_types, entry_types, entry_estimated_size)
                && is_expression_fit_for_for_loop_join(
                    b,
                    input_types,
                    entry_types,
                    entry_estimated_size,
                )
        }
        Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
            is_expression_fit_for_for_loop_join(e, input_types, entry_types, entry_estimated_size)
        }
        Expression::If(a, b, c) => {
            is_expression_fit_for_for_loop_join(a, input_types, entry_types, entry_estimated_size)
                && is_expression_fit_for_for_loop_join(
                    b,
                    input_types,
                    entry_types,
                    entry_estimated_size,
                )
                && is_expression_fit_for_for_loop_join(
                    c,
                    input_types,
                    entry_types,
                    entry_estimated_size,
                )
        }
        Expression::Exists(inner) => {
            is_fit_for_for_loop_join(inner, input_types, entry_types, entry_estimated_size)
        }
    }
}

fn has_common_variables(
    left: &VariableTypes,
    right: &VariableTypes,
    input_types: &VariableTypes,
) -> bool {
    // TODO: we should be smart and count as shared variables FILTER(?a = ?b)
    left.iter().any(|(variable, left_type)| {
        !left_type.undef && !right.get(variable).undef && input_types.get(variable).undef
    })
}

fn join_key_variables(
    left: &VariableTypes,
    right: &VariableTypes,
    input_types: &VariableTypes,
) -> Vec<Variable> {
    left.iter()
        .filter(|(variable, left_type)| {
            !left_type.undef && !right.get(variable).undef && input_types.get(variable).undef
        })
        .map(|(variable, _)| variable.clone())
        .collect()
}

fn split_singleton_union_branches(pattern: &GraphPattern) -> Option<(usize, Vec<GraphPattern>)> {
    if let GraphPattern::Union { inner } = pattern {
        let mut singleton_branch_count = 0;
        let mut non_singleton_branches = Vec::new();
        for branch in inner {
            if branch.is_empty_singleton() {
                singleton_branch_count += 1;
            } else {
                non_singleton_branches.push(branch.clone());
            }
        }
        if singleton_branch_count > 0 {
            return Some((singleton_branch_count, non_singleton_branches));
        }
    }
    None
}

fn can_deduplicate_join_factor(pattern: &GraphPattern) -> bool {
    matches!(
        pattern,
        GraphPattern::QuadPattern { .. } | GraphPattern::Path { .. } | GraphPattern::Graph { .. }
    )
}

fn flatten_inner_join_factors(pattern: GraphPattern, factors: &mut Vec<GraphPattern>) {
    if let GraphPattern::Join { left, right, .. } = pattern {
        flatten_inner_join_factors(*left, factors);
        flatten_inner_join_factors(*right, factors);
    } else {
        factors.push(pattern);
    }
}

fn factor_common_join_factors_from_union(branches: Vec<GraphPattern>) -> GraphPattern {
    if branches.len() < 2 {
        return GraphPattern::union_all(branches);
    }
    let mut branch_factors = Vec::with_capacity(branches.len());
    for branch in branches {
        let mut factors = Vec::new();
        flatten_inner_join_factors(branch, &mut factors);
        branch_factors.push(factors);
    }

    let mut common = Vec::new();
    for factor in &branch_factors[0] {
        if !can_deduplicate_join_factor(factor)
            || common.iter().any(|candidate| candidate == factor)
        {
            continue;
        }
        if branch_factors
            .iter()
            .skip(1)
            .all(|factors| factors.iter().any(|candidate| candidate == factor))
        {
            common.push(factor.clone());
        }
    }
    if common.is_empty() {
        return GraphPattern::union_all(
            branch_factors
                .into_iter()
                .map(|factors| {
                    join_all_factors(factors).unwrap_or_else(GraphPattern::empty_singleton)
                })
                .collect::<Vec<_>>(),
        );
    }

    let reduced_branches = branch_factors
        .into_iter()
        .map(|mut factors| {
            for factor in &common {
                if let Some(position) = factors.iter().position(|candidate| candidate == factor) {
                    factors.remove(position);
                }
            }
            join_all_factors(factors).unwrap_or_else(GraphPattern::empty_singleton)
        })
        .collect::<Vec<_>>();

    common.into_iter().fold(
        GraphPattern::union_all(reduced_branches),
        |accumulator, factor| {
            GraphPattern::join(
                accumulator,
                factor,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            )
        },
    )
}

fn rewrite_inner_join_with_singleton_union_branch(
    left: GraphPattern,
    right: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    if let Some((singleton_branch_count, non_singleton_branches)) =
        split_singleton_union_branches(&right)
    {
        if singleton_branch_count == 1 {
            let reordered_left = Optimizer::reorder_joins(left.clone(), input_types);
            if should_avoid_singleton_union_duplication(&left, input_types)
                || should_avoid_singleton_union_duplication(&reordered_left, input_types)
            {
                return None;
            }
            if non_singleton_branches.is_empty() {
                return Some(left);
            }
            let right_without_singleton = GraphPattern::union_all(non_singleton_branches);
            let left_types = infer_graph_pattern_types(&left, input_types.clone());
            let right_types =
                infer_graph_pattern_types(&right_without_singleton, input_types.clone());
            return Some(GraphPattern::union_all([
                left.clone(),
                GraphPattern::join(
                    left,
                    right_without_singleton,
                    JoinAlgorithm::HashBuildLeftProbeRight {
                        keys: join_key_variables(&left_types, &right_types, input_types),
                    },
                ),
            ]));
        }
    }
    if let Some((singleton_branch_count, non_singleton_branches)) =
        split_singleton_union_branches(&left)
    {
        if singleton_branch_count == 1 {
            let reordered_right = Optimizer::reorder_joins(right.clone(), input_types);
            if should_avoid_singleton_union_duplication(&right, input_types)
                || should_avoid_singleton_union_duplication(&reordered_right, input_types)
            {
                return None;
            }
            if non_singleton_branches.is_empty() {
                return Some(right);
            }
            let left_without_singleton = GraphPattern::union_all(non_singleton_branches);
            let left_types =
                infer_graph_pattern_types(&left_without_singleton, input_types.clone());
            let right_types = infer_graph_pattern_types(&right, input_types.clone());
            return Some(GraphPattern::union_all([
                right.clone(),
                GraphPattern::join(
                    left_without_singleton,
                    right,
                    JoinAlgorithm::HashBuildLeftProbeRight {
                        keys: join_key_variables(&left_types, &right_types, input_types),
                    },
                ),
            ]));
        }
    }
    None
}

fn guarded_singleton_union_inner_join_fallback(
    left: GraphPattern,
    right: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    let left = Optimizer::reorder_joins(left, input_types);
    let right = Optimizer::reorder_joins(right, input_types);
    let left_types = infer_graph_pattern_types(&left, input_types.clone());
    let right_types = infer_graph_pattern_types(&right, input_types.clone());
    if split_singleton_union_branches(&right).is_some_and(|(singleton_branch_count, _)| {
        singleton_branch_count == 1
            && should_avoid_singleton_union_duplication(&left, input_types)
    }) {
        return Some(GraphPattern::join(
            left,
            right,
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: join_key_variables(&left_types, &right_types, input_types),
            },
        ));
    }
    if split_singleton_union_branches(&left).is_some_and(|(singleton_branch_count, _)| {
        singleton_branch_count == 1
            && should_avoid_singleton_union_duplication(&right, input_types)
    }) {
        return Some(GraphPattern::join(
            left,
            right,
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: join_key_variables(&left_types, &right_types, input_types),
            },
        ));
    }
    None
}

fn should_avoid_singleton_union_duplication(
    pattern: &GraphPattern,
    input_types: &VariableTypes,
) -> bool {
    graph_pattern_contains_small_seeded_subclass_lookup_lateral(pattern, input_types)
}

fn graph_pattern_contains_small_seeded_subclass_lookup_lateral(
    pattern: &GraphPattern,
    input_types: &VariableTypes,
) -> bool {
    match pattern {
        GraphPattern::Join { left, right, .. }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Minus { left, right, .. } => {
            graph_pattern_contains_small_seeded_subclass_lookup_lateral(left, input_types)
                || graph_pattern_contains_small_seeded_subclass_lookup_lateral(right, input_types)
        }
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { left, right } => {
            let left_types = infer_graph_pattern_types(left, input_types.clone());
            let right_has_small_seeded_subclass_lookup = matches!(
                right.as_ref(),
                GraphPattern::Path {
                    subject,
                    path,
                    object,
                    ..
                } if bound_entry_variable_for_rdfs_subclass_of_kleene_transitive_path(
                    path,
                    subject,
                    object,
                    &left_types,
                )
                .is_some_and(|entry_variable| {
                    graph_pattern_has_small_seeded_lookup_for_variable(
                        left,
                        entry_variable,
                        input_types,
                    )
                })
            );
            right_has_small_seeded_subclass_lookup
                || graph_pattern_contains_small_seeded_subclass_lookup_lateral(left, input_types)
                || graph_pattern_contains_small_seeded_subclass_lookup_lateral(right, &left_types)
        }
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Group { inner, .. } => {
            graph_pattern_contains_small_seeded_subclass_lookup_lateral(inner, input_types)
        }
        GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Service { inner, .. } => {
            graph_pattern_contains_small_seeded_subclass_lookup_lateral(inner, input_types)
        }
        GraphPattern::Union { inner } => inner
            .iter()
            .any(|branch| {
                graph_pattern_contains_small_seeded_subclass_lookup_lateral(branch, input_types)
            }),
        GraphPattern::Path { .. }
        | GraphPattern::QuadPattern { .. }
        | GraphPattern::Values { .. }
        | GraphPattern::Graph { .. } => false,
    }
}

#[cfg(feature = "sep-0006")]
fn graph_pattern_has_small_seeded_lookup_for_variable(
    pattern: &GraphPattern,
    variable: &Variable,
    input_types: &VariableTypes,
) -> bool {
    match pattern {
        GraphPattern::Join { left, right, .. }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Minus { left, right, .. } => {
            graph_pattern_has_small_seeded_lookup_for_variable(left, variable, input_types)
                || graph_pattern_has_small_seeded_lookup_for_variable(right, variable, input_types)
        }
        GraphPattern::Lateral { left, right } => {
            let left_types = infer_graph_pattern_types(left, input_types.clone());
            let right_types = infer_graph_pattern_types(right, left_types.clone());
            let right_is_small_seeded_lookup =
                matches!(right.as_ref(), GraphPattern::QuadPattern { .. })
                    && left_types.get(variable).undef
                    && !right_types.get(variable).undef
                    && estimate_graph_pattern_size(left, input_types)
                        <= subclass_transitive_lateral_max_left_size()
                    && estimate_graph_pattern_size(right, &left_types)
                        <= subclass_transitive_lateral_max_left_size();
            right_is_small_seeded_lookup
                || graph_pattern_has_small_seeded_lookup_for_variable(left, variable, input_types)
                || graph_pattern_has_small_seeded_lookup_for_variable(
                    right,
                    variable,
                    &left_types,
                )
        }
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Group { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Service { inner, .. } => {
            graph_pattern_has_small_seeded_lookup_for_variable(inner, variable, input_types)
        }
        GraphPattern::Union { inner } => inner.iter().any(|branch| {
            graph_pattern_has_small_seeded_lookup_for_variable(branch, variable, input_types)
        }),
        GraphPattern::Path { .. }
        | GraphPattern::QuadPattern { .. }
        | GraphPattern::Values { .. }
        | GraphPattern::Graph { .. } => false,
    }
}

#[cfg(feature = "sep-0006")]
fn bound_entry_variable_for_rdfs_subclass_of_kleene_transitive_path<'a>(
    path: &PropertyPathExpression,
    subject: &'a GroundTermPattern,
    object: &'a GroundTermPattern,
    input_types: &VariableTypes,
) -> Option<&'a Variable> {
    match rdfs_subclass_of_kleene_transitive_path_bound_endpoint(path) {
        Some(TransitivePathBoundEndpoint::Start) if is_term_pattern_bound(subject, input_types) => {
            variable_in_term_pattern(subject)
        }
        Some(TransitivePathBoundEndpoint::End) if is_term_pattern_bound(object, input_types) => {
            variable_in_term_pattern(object)
        }
        _ => None,
    }
}

#[cfg(feature = "sep-0006")]
fn variable_in_term_pattern(pattern: &GroundTermPattern) -> Option<&Variable> {
    match pattern {
        GroundTermPattern::Variable(variable) => Some(variable),
        GroundTermPattern::NamedNode(_) | GroundTermPattern::Literal(_) => None,
        #[cfg(feature = "sparql-12")]
        GroundTermPattern::Triple(_) => None,
    }
}

fn rewrite_inner_join_with_small_factor_union(
    left: GraphPattern,
    right: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    match (&left, &right) {
        (GraphPattern::Union { .. }, _) => {
            distribute_small_factor_over_union(right, left, input_types)
        }
        (_, GraphPattern::Union { .. }) => {
            distribute_small_factor_over_union(left, right, input_types)
        }
        _ => None,
    }
}

fn rewrite_inner_join_with_correlated_filter_only_union_branch(
    left: GraphPattern,
    right: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    rewrite_correlated_filter_only_union_join(left.clone(), right.clone(), input_types).or_else(|| {
        rewrite_correlated_filter_only_union_join(right, left, input_types)
    })
}

fn rewrite_correlated_filter_only_union_join(
    shared: GraphPattern,
    union: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    let GraphPattern::Union { inner } = union else {
        return None;
    };

    let shared_types = infer_graph_pattern_types(&shared, input_types.clone());
    let mut rewritten_branches = Vec::new();
    let mut other_branches = Vec::new();

    for branch in inner {
        match branch {
            GraphPattern::Filter { inner, expression }
                if inner.is_empty_singleton()
                    && expression_contains_highly_correlated_filter_exists(
                        &expression,
                        &shared_types,
                    ) =>
            {
                rewritten_branches.push(GraphPattern::filter(shared.clone(), expression));
            }
            other => other_branches.push(other),
        }
    }

    if rewritten_branches.is_empty() {
        return None;
    }

    if !other_branches.is_empty() {
        let other_union = GraphPattern::union_all(other_branches);
        let other_types = infer_graph_pattern_types(&other_union, input_types.clone());
        rewritten_branches.push(GraphPattern::join(
            shared,
            other_union,
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: join_key_variables(&shared_types, &other_types, input_types),
            },
        ));
    }

    Some(GraphPattern::union_all(rewritten_branches))
}

fn rewrite_flat_join_with_small_factor_union(
    factors: Vec<GraphPattern>,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    if factors.len() < 3 {
        return None;
    }

    let union_index = factors.iter().enumerate().find_map(|(idx, factor)| {
        let GraphPattern::Union { inner } = factor else {
            return None;
        };
        if inner.len() < 2 || inner.len() > join_union_distribution_max_branches() {
            return None;
        }
        if !inner
            .iter()
            .any(|branch| has_local_cartesian_inner_join(branch, input_types))
        {
            return None;
        }
        Some(idx)
    })?;

    let GraphPattern::Union { inner: branches } = factors[union_index].clone() else {
        return None;
    };

    let factor_index = factors
        .iter()
        .enumerate()
        .filter_map(|(idx, factor)| {
            if idx == union_index || !is_small_factor_for_union_distribution(factor, input_types) {
                return None;
            }
            let factor_types = infer_graph_pattern_types(factor, input_types.clone());
            let mut total_key_count: usize = 0;
            for branch in &branches {
                let key_count = join_key_variables(
                    &factor_types,
                    &infer_graph_pattern_types(branch, input_types.clone()),
                    input_types,
                )
                .len();
                if key_count == 0 {
                    return None;
                }
                total_key_count = total_key_count.saturating_add(key_count);
            }
            Some((
                idx,
                total_key_count,
                estimate_graph_pattern_size(factor, input_types),
            ))
        })
        .max_by(|(_, left_keys, left_size), (_, right_keys, right_size)| {
            left_keys
                .cmp(right_keys)
                .then_with(|| right_size.cmp(left_size))
        })?
        .0;

    let factor = factors[factor_index].clone();
    let base_factors = factors
        .into_iter()
        .enumerate()
        .filter_map(|(idx, factor)| {
            if idx == union_index || idx == factor_index {
                None
            } else {
                Some(factor)
            }
        })
        .collect::<Vec<_>>();

    let factor_types = infer_graph_pattern_types(&factor, input_types.clone());
    let distributed_union = GraphPattern::union_all(branches.into_iter().map(|branch| {
        let branch_types = infer_graph_pattern_types(&branch, input_types.clone());
        GraphPattern::join(
            factor.clone(),
            branch,
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: join_key_variables(&factor_types, &branch_types, input_types),
            },
        )
    }));
    let rewritten = if base_factors.is_empty() {
        distributed_union
    } else {
        let mut rewritten_factors = base_factors;
        rewritten_factors.push(distributed_union);
        join_all_factors(rewritten_factors).expect("rewritten factors should never be empty")
    };
    Some(rewritten)
}

fn rewrite_flat_join_with_guarded_singleton_union_branch(
    base_factors: Vec<GraphPattern>,
    non_singleton_branches: Vec<GraphPattern>,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    let right_without_singleton = GraphPattern::union_all(non_singleton_branches);
    let base_join = join_all_factors(base_factors.clone())?;
    let base_types = infer_graph_pattern_types(&base_join, input_types.clone());
    let right_types = infer_graph_pattern_types(&right_without_singleton, input_types.clone());
    let shared_keys = join_key_variables(&base_types, &right_types, input_types);
    if shared_keys.is_empty() {
        return None;
    }

    let factor_types = base_factors
        .iter()
        .map(|factor| infer_graph_pattern_types(factor, input_types.clone()))
        .collect::<Vec<_>>();
    let guarded_flags = base_factors
        .iter()
        .map(|factor| should_avoid_singleton_union_duplication(factor, input_types))
        .collect::<Vec<_>>();

    let mut remaining_keys = shared_keys.into_iter().collect::<HashSet<_>>();
    let mut duplicated_indices = Vec::new();
    let mut duplicated_has_unguarded_factor = false;

    while !remaining_keys.is_empty() {
        let next_index = base_factors
            .iter()
            .enumerate()
            .filter(|(idx, _)| !duplicated_indices.contains(idx))
            .filter_map(|(idx, factor)| {
                let covered_keys = remaining_keys
                    .iter()
                    .filter(|variable| !factor_types[idx].get(variable).undef)
                    .count();
                (covered_keys > 0).then_some((
                    idx,
                    covered_keys,
                    guarded_flags[idx],
                    estimate_graph_pattern_size(factor, input_types),
                ))
            })
            .max_by(|(_, left_covered, left_guarded, left_cost), (_, right_covered, right_guarded, right_cost)| {
                left_covered
                    .cmp(right_covered)
                    .then_with(|| (!*left_guarded).cmp(&!*right_guarded))
                    .then_with(|| right_cost.cmp(left_cost))
            })?
            .0;

        duplicated_indices.push(next_index);
        duplicated_has_unguarded_factor |= !guarded_flags[next_index];
        remaining_keys.retain(|variable| factor_types[next_index].get(variable).undef);
    }

    if !duplicated_has_unguarded_factor {
        return None;
    }

    let mut duplicated_factors = Vec::new();
    let mut shared_factors = Vec::new();
    for (idx, factor) in base_factors.into_iter().enumerate() {
        if duplicated_indices.contains(&idx) {
            duplicated_factors.push(factor);
        } else {
            shared_factors.push(factor);
        }
    }

    let duplicated_join = join_all_factors(duplicated_factors.clone())?;
    let branch_join = join_all_factors({
        let mut branch_factors = duplicated_factors;
        branch_factors.push(right_without_singleton);
        branch_factors
    })?;
    let rewritten_union = GraphPattern::union_all([duplicated_join, branch_join]);

    if shared_factors.is_empty() {
        Some(rewritten_union)
    } else {
        let mut outer_factors = shared_factors;
        outer_factors.push(rewritten_union);
        join_all_factors(outer_factors)
    }
}

fn distribute_small_factor_over_union(
    factor: GraphPattern,
    union: GraphPattern,
    input_types: &VariableTypes,
) -> Option<GraphPattern> {
    if !is_small_factor_for_union_distribution(&factor, input_types) {
        return None;
    }

    let GraphPattern::Union { inner } = union else {
        return None;
    };
    if inner.len() < 2 || inner.len() > join_union_distribution_max_branches() {
        return None;
    }
    if !inner
        .iter()
        .any(|branch| has_local_cartesian_inner_join(branch, input_types))
    {
        return None;
    }
    let factor_types = infer_graph_pattern_types(&factor, input_types.clone());
    let mut rewritten_branches = Vec::with_capacity(inner.len());
    for branch in inner {
        let branch_types = infer_graph_pattern_types(&branch, input_types.clone());
        let keys = join_key_variables(&factor_types, &branch_types, input_types);
        if keys.is_empty() {
            return None;
        }
        rewritten_branches.push(GraphPattern::join(
            factor.clone(),
            branch,
            JoinAlgorithm::HashBuildLeftProbeRight { keys },
        ));
    }
    Some(GraphPattern::union_all(rewritten_branches))
}

fn is_small_factor_for_union_distribution(
    pattern: &GraphPattern,
    input_types: &VariableTypes,
) -> bool {
    matches!(
        pattern,
        GraphPattern::QuadPattern { .. } | GraphPattern::Path { .. }
    ) && estimate_graph_pattern_size(pattern, input_types)
        <= join_union_distribution_max_factor_cost()
}

fn has_local_cartesian_inner_join(pattern: &GraphPattern, input_types: &VariableTypes) -> bool {
    match pattern {
        GraphPattern::Join { left, right, .. } => {
            let left_types = infer_graph_pattern_types(left, input_types.clone());
            let right_types = infer_graph_pattern_types(right, input_types.clone());
            join_key_variables(&left_types, &right_types, input_types).is_empty()
        }
        GraphPattern::LeftJoin { .. } | GraphPattern::Minus { .. } => false,
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { .. } => false,
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner, .. }
        | GraphPattern::Reduced { inner, .. }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. }
        | GraphPattern::Service { inner, .. } => has_local_cartesian_inner_join(inner, input_types),
        GraphPattern::Union { .. } => false,
        GraphPattern::Values { .. }
        | GraphPattern::QuadPattern { .. }
        | GraphPattern::Path { .. }
        | GraphPattern::Graph { .. } => false,
    }
}

fn join_all_factors(factors: Vec<GraphPattern>) -> Option<GraphPattern> {
    let mut factors = factors.into_iter();
    let first = factors.next()?;
    Some(factors.fold(first, |left, right| {
        GraphPattern::join(
            left,
            right,
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        )
    }))
}

fn estimate_graph_pattern_size(pattern: &GraphPattern, input_types: &VariableTypes) -> usize {
    match pattern {
        GraphPattern::Values { bindings, .. } => bindings.len(),
        GraphPattern::QuadPattern {
            subject,
            predicate,
            object,
            ..
        } => estimate_triple_pattern_size(
            is_term_pattern_bound(subject, input_types),
            is_named_node_pattern_bound(predicate, input_types),
            is_term_pattern_bound(object, input_types),
        ),
        GraphPattern::Path {
            subject,
            path,
            object,
            ..
        } => estimate_path_size(
            is_term_pattern_bound(subject, input_types),
            path,
            is_term_pattern_bound(object, input_types),
        ),
        GraphPattern::Graph { graph_name } => {
            if is_named_node_pattern_bound(graph_name, input_types) {
                100
            } else {
                1
            }
        }
        GraphPattern::Join {
            left,
            right,
            algorithm,
        } => estimate_join_cost(left, right, algorithm, input_types),
        GraphPattern::LeftJoin {
            left,
            right,
            algorithm,
            ..
        } => match algorithm {
            LeftJoinAlgorithm::HashBuildRightProbeLeft { keys } => {
                let left_size = estimate_graph_pattern_size(left, input_types);
                max(
                    left_size,
                    left_size
                        .saturating_mul(estimate_graph_pattern_size(
                            right,
                            &infer_graph_pattern_types(right, input_types.clone()),
                        ))
                        .saturating_div(1_000_usize.saturating_pow(keys.len().try_into().unwrap())),
                )
            }
        },
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { left, right } => estimate_lateral_cost(
            left,
            &infer_graph_pattern_types(left, input_types.clone()),
            right,
            input_types,
        ),
        GraphPattern::Union { inner } => inner
            .iter()
            .map(|inner| estimate_graph_pattern_size(inner, input_types))
            .fold(0, usize::saturating_add),
        GraphPattern::Minus { left, .. } => estimate_graph_pattern_size(left, input_types),
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner, .. }
        | GraphPattern::Reduced { inner, .. }
        | GraphPattern::Group { inner, .. } => estimate_graph_pattern_size(inner, input_types),
        GraphPattern::Service { name, inner, .. } => {
            if let NamedNodePattern::Variable(name) = name {
                if input_types.get(name).undef {
                    // Avoid planning SERVICE before the endpoint variable gets bound.
                    return 1_000_000_000;
                }
            }
            estimate_graph_pattern_size(inner, input_types)
        }
        GraphPattern::Slice {
            inner,
            start,
            length,
        } => {
            let inner = estimate_graph_pattern_size(inner, input_types);
            if let Some(length) = length {
                min(inner, *length - *start)
            } else {
                inner
            }
        }
    }
}

#[cfg(feature = "sep-0006")]
fn estimate_for_loop_entry_size(pattern: &GraphPattern, input_types: &VariableTypes) -> usize {
    let estimate = estimate_graph_pattern_size(pattern, input_types);
    if graph_pattern_contains_small_seeded_subclass_lookup_lateral(pattern, input_types) {
        min(estimate, subclass_transitive_lateral_max_left_size())
    } else {
        estimate
    }
}

#[cfg(not(feature = "sep-0006"))]
fn estimate_for_loop_entry_size(pattern: &GraphPattern, input_types: &VariableTypes) -> usize {
    estimate_graph_pattern_size(pattern, input_types)
}

fn estimate_join_cost(
    left: &GraphPattern,
    right: &GraphPattern,
    algorithm: &JoinAlgorithm,
    input_types: &VariableTypes,
) -> usize {
    match algorithm {
        JoinAlgorithm::HashBuildLeftProbeRight { keys } => {
            estimate_graph_pattern_size(left, input_types)
                .saturating_mul(estimate_graph_pattern_size(right, input_types))
                .saturating_div(1_000_usize.saturating_pow(keys.len().try_into().unwrap()))
        }
    }
}
fn estimate_lateral_cost(
    left: &GraphPattern,
    left_types: &VariableTypes,
    right: &GraphPattern,
    input_types: &VariableTypes,
) -> usize {
    estimate_graph_pattern_size(left, input_types)
        .saturating_mul(estimate_graph_pattern_size(right, left_types))
}

fn estimate_triple_pattern_size(
    subject_bound: bool,
    predicate_bound: bool,
    object_bound: bool,
) -> usize {
    match (subject_bound, predicate_bound, object_bound) {
        (true, true, true) => 1,
        (true, true, false) => 10,
        (true, false, true) => 2,
        (false, true, true) => 10_000,
        (true, false, false) => 100,
        (false, false, false) => 1_000_000_000,
        (false, true, false) => 1_000_000,
        (false, false, true) => 100_000,
    }
}

fn expression_contains_highly_correlated_filter_exists(
    expression: &Expression,
    input_types: &VariableTypes,
) -> bool {
    match expression {
        Expression::Exists(inner) => should_reorder_highly_correlated_filter_exists(inner, input_types),
        Expression::Or(inner) | Expression::And(inner) | Expression::Coalesce(inner) => inner
            .iter()
            .any(|expression| {
                expression_contains_highly_correlated_filter_exists(expression, input_types)
            }),
        Expression::Equal(left, right)
        | Expression::SameTerm(left, right)
        | Expression::Greater(left, right)
        | Expression::GreaterOrEqual(left, right)
        | Expression::Less(left, right)
        | Expression::LessOrEqual(left, right)
        | Expression::Add(left, right)
        | Expression::Subtract(left, right)
        | Expression::Multiply(left, right)
        | Expression::Divide(left, right) => {
            expression_contains_highly_correlated_filter_exists(left, input_types)
                || expression_contains_highly_correlated_filter_exists(right, input_types)
        }
        Expression::UnaryPlus(inner)
        | Expression::UnaryMinus(inner)
        | Expression::Not(inner) => {
            expression_contains_highly_correlated_filter_exists(inner, input_types)
        }
        Expression::If(condition, then, els) => {
            expression_contains_highly_correlated_filter_exists(condition, input_types)
                || expression_contains_highly_correlated_filter_exists(then, input_types)
                || expression_contains_highly_correlated_filter_exists(els, input_types)
        }
        Expression::FunctionCall(_, arguments) => arguments.iter().any(|argument| {
            expression_contains_highly_correlated_filter_exists(argument, input_types)
        }),
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => false,
    }
}

fn estimate_path_size(start_bound: bool, path: &PropertyPathExpression, end_bound: bool) -> usize {
    match path {
        PropertyPathExpression::NamedNode(_) => {
            estimate_triple_pattern_size(start_bound, true, end_bound)
        }
        PropertyPathExpression::Reverse(p) => estimate_path_size(end_bound, p, start_bound),
        PropertyPathExpression::Sequence(a, b) => {
            // We do a for loop join in the best direction
            min(
                estimate_path_size(start_bound, a, false)
                    .saturating_mul(estimate_path_size(true, b, end_bound)),
                estimate_path_size(start_bound, a, true)
                    .saturating_mul(estimate_path_size(false, b, end_bound)),
            )
        }
        PropertyPathExpression::Alternative(a, b) => estimate_path_size(start_bound, a, end_bound)
            .saturating_add(estimate_path_size(start_bound, b, end_bound)),
        PropertyPathExpression::ZeroOrMore(p) => {
            if start_bound && end_bound {
                1
            } else if start_bound || end_bound {
                if is_entry_bound_for_rdfs_subclass_of_kleene_transitive_path(
                    path,
                    start_bound,
                    end_bound,
                ) {
                    estimate_path_size(start_bound, p, end_bound)
                } else {
                    max(
                        estimate_path_size(start_bound, p, end_bound)
                            .saturating_mul(open_transitive_path_factor()),
                        open_transitive_path_min_cost(),
                    )
                }
            } else {
                open_transitive_path_full_scan_cost()
            }
        }
        PropertyPathExpression::OneOrMore(p) => {
            if start_bound && end_bound {
                1
            } else if start_bound || end_bound {
                if is_entry_bound_for_rdfs_subclass_of_kleene_transitive_path(
                    path,
                    start_bound,
                    end_bound,
                ) {
                    estimate_path_size(start_bound, p, end_bound)
                } else {
                    max(
                        estimate_path_size(start_bound, p, end_bound)
                            .saturating_mul(open_transitive_path_factor()),
                        open_transitive_path_min_cost(),
                    )
                }
            } else {
                max(
                    estimate_path_size(start_bound, p, end_bound)
                        .saturating_mul(open_transitive_path_factor()),
                    open_transitive_path_full_scan_cost(),
                )
            }
        }
        PropertyPathExpression::ZeroOrOne(p) => {
            if start_bound && end_bound {
                1
            } else if start_bound || end_bound {
                estimate_path_size(start_bound, p, end_bound)
            } else {
                1_000_000_000
            }
        }
        PropertyPathExpression::NegatedPropertySet(_) => {
            estimate_triple_pattern_size(start_bound, false, end_bound)
        }
    }
}

fn graph_pattern_reordering_penalty(pattern: &GraphPattern, input_types: &VariableTypes) -> usize {
    match pattern {
        GraphPattern::Path {
            subject,
            path,
            object,
            ..
        } => path_reordering_penalty(
            is_term_pattern_bound(subject, input_types),
            path,
            is_term_pattern_bound(object, input_types),
        ),
        GraphPattern::QuadPattern { .. } | GraphPattern::Values { .. } => 0,
        GraphPattern::Graph { .. } => 1,
        GraphPattern::Join { .. }
        | GraphPattern::LeftJoin { .. }
        | GraphPattern::Minus { .. }
        | GraphPattern::Filter { .. }
        | GraphPattern::Union { .. }
        | GraphPattern::Extend { .. }
        | GraphPattern::OrderBy { .. }
        | GraphPattern::Project { .. }
        | GraphPattern::Distinct { .. }
        | GraphPattern::Reduced { .. }
        | GraphPattern::Slice { .. }
        | GraphPattern::Group { .. }
        | GraphPattern::Service { .. } => 1,
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { .. } => 1,
    }
}

fn path_reordering_penalty(
    start_bound: bool,
    path: &PropertyPathExpression,
    end_bound: bool,
) -> usize {
    match path {
        PropertyPathExpression::NamedNode(_) => 0,
        PropertyPathExpression::Reverse(p) => path_reordering_penalty(end_bound, p, start_bound),
        PropertyPathExpression::Sequence(a, b) => max(
            path_reordering_penalty(start_bound, a, false),
            path_reordering_penalty(true, b, end_bound),
        ),
        PropertyPathExpression::Alternative(a, b) => max(
            path_reordering_penalty(start_bound, a, end_bound),
            path_reordering_penalty(start_bound, b, end_bound),
        ),
        PropertyPathExpression::ZeroOrMore(_) | PropertyPathExpression::OneOrMore(_) => {
            if start_bound && end_bound {
                1
            } else if start_bound || end_bound {
                2
            } else {
                3
            }
        }
        PropertyPathExpression::ZeroOrOne(p) => path_reordering_penalty(start_bound, p, end_bound),
        PropertyPathExpression::NegatedPropertySet(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    fn transitive_subclass_path_star() -> PropertyPathExpression {
        PropertyPathExpression::ZeroOrMore(Box::new(PropertyPathExpression::NamedNode(
            NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
        )))
    }

    fn transitive_subclass_path_plus() -> PropertyPathExpression {
        PropertyPathExpression::OneOrMore(Box::new(PropertyPathExpression::NamedNode(
            NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
        )))
    }

    fn reverse_transitive_subclass_path_plus() -> PropertyPathExpression {
        PropertyPathExpression::OneOrMore(Box::new(PropertyPathExpression::Reverse(Box::new(
            PropertyPathExpression::NamedNode(NamedNode::new_unchecked(RDFS_SUBCLASS_OF_IRI)),
        ))))
    }

    #[test]
    fn generic_open_transitive_path_cost_is_strongly_penalized() {
        let star = PropertyPathExpression::ZeroOrMore(Box::new(PropertyPathExpression::NamedNode(
            NamedNode::new_unchecked("http://example.com/p"),
        )));
        let plus = PropertyPathExpression::OneOrMore(Box::new(PropertyPathExpression::NamedNode(
            NamedNode::new_unchecked("http://example.com/p"),
        )));
        let baseline_selective_triple = estimate_triple_pattern_size(false, true, true);
        assert!(
            estimate_path_size(true, &star, false) > baseline_selective_triple,
            "p* with one bound endpoint should be costlier than a selective triple pattern"
        );
        assert!(
            estimate_path_size(false, &plus, true) > baseline_selective_triple,
            "p+ with one bound endpoint should be costlier than a selective triple pattern"
        );
    }

    #[test]
    fn anchored_subclass_transitive_path_cost_stays_near_selective_triple() {
        let star = transitive_subclass_path_star();
        let plus = transitive_subclass_path_plus();
        let anchored_subclass_triple = estimate_triple_pattern_size(true, true, false);

        assert_eq!(
            estimate_path_size(true, &star, false),
            anchored_subclass_triple
        );
        assert_eq!(
            estimate_path_size(true, &plus, false),
            anchored_subclass_triple
        );
        assert_eq!(
            estimate_path_size(false, &reverse_transitive_subclass_path_plus(), true),
            anchored_subclass_triple
        );
    }

    #[test]
    fn unanchored_direction_subclass_transitive_path_keeps_open_path_penalty() {
        let star = transitive_subclass_path_star();
        let plus = transitive_subclass_path_plus();
        let baseline_selective_triple = estimate_triple_pattern_size(false, true, true);

        assert!(
            estimate_path_size(false, &star, true) > baseline_selective_triple,
            "forward subclass traversal with only the end bound should keep the generic open-path penalty",
        );
        assert!(
            estimate_path_size(false, &plus, true) > baseline_selective_triple,
            "forward subclass traversal with only the end bound should keep the generic open-path penalty",
        );
    }

    #[test]
    fn closed_transitive_path_cost_stays_small() {
        let star = transitive_subclass_path_star();
        let plus = transitive_subclass_path_plus();
        assert_eq!(estimate_path_size(true, &star, true), 1);
        assert_eq!(estimate_path_size(true, &plus, true), 1);
    }

    #[test]
    fn bounded_transitive_path_can_use_for_loop_when_entry_is_small() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: PropertyPathExpression::OneOrMore(Box::new(PropertyPathExpression::NamedNode(
                NamedNode::new_unchecked("http://example.com/p"),
            ))),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: NamedNode::new_unchecked("http://example.com/o").into(),
            graph_name: None,
        };
        let global_types = VariableTypes::default();
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());

        assert!(
            is_fit_for_for_loop_join(&transitive_path, &global_types, &entry_types, 1),
            "bounded transitive path should allow for-loop join for tiny left inputs",
        );
        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &global_types,
                &entry_types,
                transitive_lateral_max_left_size().saturating_add(1),
            ),
            "bounded transitive path should not use for-loop join once left input grows too much",
        );
    }

    #[test]
    fn subclass_transitive_path_can_use_attack_id_sized_anchor() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let global_types = VariableTypes::default();
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());
        let entry_estimated_size = estimate_graph_pattern_size(&entry_seed, &global_types);

        assert_eq!(
            entry_estimated_size,
            estimate_triple_pattern_size(false, true, true),
            "attack-id style anchors should keep the object-bound triple estimate used by the planner",
        );
        assert!(
            is_fit_for_for_loop_join(
                &transitive_path,
                &global_types,
                &entry_types,
                entry_estimated_size,
            ),
            "transitive rdfs:subClassOf paths should stay lateralizable for attack-id sized anchors",
        );
        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &global_types,
                &entry_types,
                subclass_transitive_lateral_max_left_size().saturating_add(1),
            ),
            "subclass transitive paths should still stop lateralizing above the dedicated threshold",
        );
    }

    #[test]
    fn generic_transitive_path_stays_blocked_for_attack_id_sized_anchor() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: PropertyPathExpression::OneOrMore(Box::new(PropertyPathExpression::NamedNode(
                NamedNode::new_unchecked("http://example.com/p"),
            ))),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());
        let entry_estimated_size =
            estimate_graph_pattern_size(&entry_seed, &VariableTypes::default());

        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &VariableTypes::default(),
                &entry_types,
                entry_estimated_size,
            ),
            "the broader subclass threshold must not make arbitrary transitive predicates lateralizable",
        );
    }

    #[test]
    fn forward_subclass_transitive_path_stays_blocked_when_only_end_is_entry_bound() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("o").into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());
        let entry_estimated_size =
            estimate_graph_pattern_size(&entry_seed, &VariableTypes::default());

        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &VariableTypes::default(),
                &entry_types,
                entry_estimated_size,
            ),
            "forward rdfs:subClassOf traversal should not relax the threshold when only the path end is entry-bound",
        );
    }

    #[test]
    fn reverse_subclass_transitive_path_can_use_attack_id_sized_anchor() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: reverse_transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("o").into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let global_types = VariableTypes::default();
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());
        let entry_estimated_size = estimate_graph_pattern_size(&entry_seed, &global_types);

        assert!(
            is_fit_for_for_loop_join(
                &transitive_path,
                &global_types,
                &entry_types,
                entry_estimated_size,
            ),
            "reverse rdfs:subClassOf traversal should relax the threshold when the path end is entry-bound",
        );
    }

    #[test]
    fn reverse_subclass_transitive_path_stays_blocked_when_only_start_is_entry_bound() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: reverse_transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());
        let entry_estimated_size =
            estimate_graph_pattern_size(&entry_seed, &VariableTypes::default());

        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &VariableTypes::default(),
                &entry_types,
                entry_estimated_size,
            ),
            "reverse rdfs:subClassOf traversal should not relax the threshold when only the path start is entry-bound",
        );
    }

    #[test]
    fn join_branch_can_use_for_loop_when_entry_and_internal_factors_bind_in_sequence() {
        let off_tech = Variable::new_unchecked("off_tech");
        let top_level = Variable::new_unchecked("_off_top_level");
        let parent = Variable::new_unchecked("parent");
        let branch = GraphPattern::join(
            GraphPattern::join(
                GraphPattern::QuadPattern {
                    subject: parent.clone().into(),
                    predicate: NamedNode::new_unchecked("http://example.com/rel").into(),
                    object: Variable::new_unchecked("off_artifact").into(),
                    graph_name: None,
                },
                GraphPattern::Path {
                    subject: parent.clone().into(),
                    path: transitive_subclass_path_plus(),
                    object: top_level.clone().into(),
                    graph_name: None,
                },
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![parent.clone()],
                },
            ),
            GraphPattern::Path {
                subject: off_tech.clone().into(),
                path: transitive_subclass_path_plus(),
                object: parent.into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("parent")],
            },
        );
        let entry_seed = GraphPattern::extend(
            GraphPattern::extend(
                GraphPattern::empty_singleton(),
                off_tech,
                NamedNode::new_unchecked("http://example.com/off-tech").into(),
            ),
            top_level,
            NamedNode::new_unchecked("http://example.com/top-level").into(),
        );
        let global_types = VariableTypes::default();
        let entry_types = infer_graph_pattern_types(&entry_seed, global_types.clone());

        assert!(
            is_fit_for_for_loop_join(
                &branch,
                &global_types,
                &entry_types,
                estimate_graph_pattern_size(&entry_seed, &global_types),
            ),
            "join branches should be lateralizable when entry bindings plus earlier factors can bind each transitive path in sequence",
        );
    }

    #[test]
    fn transitive_path_requires_endpoint_binding_from_entry() {
        let transitive_path = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let binding_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: NamedNode::new_unchecked("http://example.com/o").into(),
            graph_name: None,
        };
        let already_bound_types =
            infer_graph_pattern_types(&binding_seed, VariableTypes::default());

        assert!(
            !is_fit_for_for_loop_join(
                &transitive_path,
                &VariableTypes::default(),
                &VariableTypes::default(),
                1,
            ),
            "for-loop join should stay disabled when the entry side does not bind any path endpoint",
        );
        assert!(
            is_fit_for_for_loop_join(
                &transitive_path,
                &already_bound_types,
                &already_bound_types,
                1
            ),
            "for-loop join is allowed when the entry side binds at least one endpoint",
        );
    }

    #[test]
    fn quad_for_loop_allows_small_unbound_quad() {
        let quad = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let entry_seed = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/bind").into(),
            object: NamedNode::new_unchecked("http://example.com/object").into(),
            graph_name: None,
        };
        let entry_types = infer_graph_pattern_types(&entry_seed, VariableTypes::default());

        assert!(
            is_fit_for_for_loop_join(
                &quad,
                &VariableTypes::default(),
                &VariableTypes::default(),
                1
            ),
            "small quad patterns may still use for-loop join even without entry-provided bindings",
        );
        assert!(
            is_fit_for_for_loop_join(&quad, &VariableTypes::default(), &entry_types, 1),
            "quad pattern should use for-loop join when entry binds one of its variables",
        );
    }

    #[test]
    fn subclass_quad_for_loop_is_disabled_for_large_left_inputs() {
        let subclass_quad = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked(RDFS_SUBCLASS_OF_IRI).into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        assert!(
            !is_fit_for_for_loop_join(
                &subclass_quad,
                &VariableTypes::default(),
                &VariableTypes::default(),
                subclass_for_loop_max_left_size().saturating_add(1),
            ),
            "large lateral loops over rdfs:subClassOf triples should be avoided",
        );
        assert!(
            is_fit_for_for_loop_join(
                &subclass_quad,
                &VariableTypes::default(),
                &VariableTypes::default(),
                subclass_for_loop_max_left_size(),
            ),
            "subClassOf quad should still be allowed at or below the threshold",
        );
    }

    #[test]
    fn non_subclass_quad_is_not_blocked_by_large_left_inputs() {
        let quad = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        assert!(
            is_fit_for_for_loop_join(
                &quad,
                &VariableTypes::default(),
                &VariableTypes::default(),
                subclass_for_loop_max_left_size().saturating_add(1),
            ),
            "the size guard should target rdfs:subClassOf quads only",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn optimizer_lateralizes_attack_id_anchored_subclass_path() {
        let off_tech = Variable::new_unchecked("off_tech");
        let framework_root = Variable::new_unchecked("framework_root_iri");
        let attack_id_quad = GraphPattern::QuadPattern {
            subject: off_tech.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let path = GraphPattern::Path {
            subject: off_tech.clone().into(),
            path: transitive_subclass_path_star(),
            object: framework_root.into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            attack_id_quad.clone(),
            path.clone(),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![off_tech.clone()],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        match optimized {
            GraphPattern::Lateral { left, right } => {
                assert_eq!(*left, attack_id_quad);
                assert_eq!(*right, path);
            }
            other => panic!(
                "optimizer should lateralize attack-id anchored subclass transitive paths, got {other:?}"
            ),
        }
    }

    #[cfg(feature = "sep-0006")]
    fn has_lateral_subtree(
        pattern: &GraphPattern,
        expected_left: &GraphPattern,
        expected_right: &GraphPattern,
    ) -> bool {
        match pattern {
            GraphPattern::Lateral { left, right } => {
                (left.as_ref() == expected_left && right.as_ref() == expected_right)
                    || has_lateral_subtree(left, expected_left, expected_right)
                    || has_lateral_subtree(right, expected_left, expected_right)
            }
            GraphPattern::Join { left, right, .. }
            | GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Minus { left, right, .. } => {
                has_lateral_subtree(left, expected_left, expected_right)
                    || has_lateral_subtree(right, expected_left, expected_right)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner, .. }
            | GraphPattern::Reduced { inner, .. }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. }
            | GraphPattern::Service { inner, .. } => {
                has_lateral_subtree(inner, expected_left, expected_right)
            }
            GraphPattern::Union { inner } => inner
                .iter()
                .any(|branch| has_lateral_subtree(branch, expected_left, expected_right)),
            GraphPattern::Values { .. }
            | GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. } => false,
        }
    }

    #[cfg(feature = "sep-0006")]
    fn has_lateral_with_union_right(pattern: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::Lateral { left, right } => {
                matches!(right.as_ref(), GraphPattern::Union { .. })
                    || has_lateral_with_union_right(left)
                    || has_lateral_with_union_right(right)
            }
            GraphPattern::Join { left, right, .. }
            | GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Minus { left, right, .. } => {
                has_lateral_with_union_right(left) || has_lateral_with_union_right(right)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner, .. }
            | GraphPattern::Reduced { inner, .. }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. }
            | GraphPattern::Service { inner, .. } => has_lateral_with_union_right(inner),
            GraphPattern::Union { inner } => inner.iter().any(has_lateral_with_union_right),
            GraphPattern::Values { .. }
            | GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. } => false,
        }
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn optimizer_keeps_attack_id_anchored_subclass_path_lateral_under_framework_root_join() {
        let off_tech = Variable::new_unchecked("off_tech");
        let framework_root = Variable::new_unchecked("framework_root_iri");
        let framework_root_key = framework_root.clone();
        let off_top_level = Variable::new_unchecked("off_top_level");
        let attack_id_quad = GraphPattern::QuadPattern {
            subject: off_tech.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
            object: Literal::new_simple_literal("T1546").into(),
            graph_name: None,
        };
        let path = GraphPattern::Path {
            subject: off_tech.clone().into(),
            path: transitive_subclass_path_star(),
            object: framework_root.clone().into(),
            graph_name: None,
        };
        let framework_root_child = GraphPattern::QuadPattern {
            subject: off_top_level.into(),
            predicate: NamedNode::new_unchecked(RDFS_SUBCLASS_OF_IRI).into(),
            object: framework_root.into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::join(
                attack_id_quad.clone(),
                path.clone(),
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![off_tech.clone()],
                },
            ),
            framework_root_child,
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![framework_root_key],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            has_lateral_subtree(&optimized, &attack_id_quad, &path),
            "optimizer should keep the attack-id anchor correlated with the subclass path even when another join consumes framework_root_iri, got {optimized:?}",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn optimizer_lateralizes_union_of_join_branches_for_small_anchor() {
        let off_tech = Variable::new_unchecked("off_tech");
        let top_level = Variable::new_unchecked("_off_top_level");
        let parent = Variable::new_unchecked("parent");
        let child = Variable::new_unchecked("child");
        let anchor = GraphPattern::extend(
            GraphPattern::extend(
                GraphPattern::empty_singleton(),
                off_tech.clone(),
                NamedNode::new_unchecked("http://example.com/off-tech").into(),
            ),
            top_level.clone(),
            NamedNode::new_unchecked("http://example.com/top-level").into(),
        );
        let branch_a = GraphPattern::join(
            GraphPattern::join(
                GraphPattern::QuadPattern {
                    subject: parent.clone().into(),
                    predicate: Variable::new_unchecked("off_artifact_rel").into(),
                    object: Variable::new_unchecked("off_artifact").into(),
                    graph_name: None,
                },
                GraphPattern::Path {
                    subject: parent.clone().into(),
                    path: transitive_subclass_path_plus(),
                    object: top_level.into(),
                    graph_name: None,
                },
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![parent.clone()],
                },
            ),
            GraphPattern::Path {
                subject: off_tech.clone().into(),
                path: transitive_subclass_path_plus(),
                object: parent.into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("parent")],
            },
        );
        let branch_b = GraphPattern::join(
            GraphPattern::QuadPattern {
                subject: child.clone().into(),
                predicate: Variable::new_unchecked("off_artifact_rel").into(),
                object: Variable::new_unchecked("off_artifact").into(),
                graph_name: None,
            },
            GraphPattern::Path {
                subject: child.into(),
                path: transitive_subclass_path_plus(),
                object: off_tech.clone().into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("child")],
            },
        );
        let pattern = GraphPattern::join(
            anchor,
            GraphPattern::union_all([branch_a, branch_b]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![off_tech],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            has_lateral_with_union_right(&optimized),
            "small anchors should lateralize unions of join branches so the union runs per entry binding, got {optimized:?}",
        );
    }

    #[cfg(feature = "sep-0006")]
    fn attack_id_anchored_subclass_lateral_shared_factor() -> GraphPattern {
        let off_tech = Variable::new_unchecked("s");
        let off_tech_id = Variable::new_unchecked("off_tech_id");
        let framework_root = Variable::new_unchecked("framework_root_iri");
        GraphPattern::lateral(
            GraphPattern::lateral(
                GraphPattern::extend(
                    GraphPattern::empty_singleton(),
                    off_tech_id.clone(),
                    Literal::new_simple_literal("T1546").into(),
                ),
                GraphPattern::QuadPattern {
                    subject: off_tech.clone().into(),
                    predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
                    object: off_tech_id.into(),
                    graph_name: None,
                },
            ),
            GraphPattern::Path {
                subject: off_tech.into(),
                path: transitive_subclass_path_star(),
                object: framework_root.into(),
                graph_name: None,
            },
        )
    }

    #[cfg(feature = "sep-0006")]
    fn singleton_anchored_subclass_lateral_factor() -> GraphPattern {
        let off_tech = Variable::new_unchecked("s");
        let framework_root = Variable::new_unchecked("framework_root_iri");
        GraphPattern::lateral(
            GraphPattern::extend(
                GraphPattern::empty_singleton(),
                off_tech.clone(),
                NamedNode::new_unchecked("http://example.com/seed").into(),
            ),
            GraphPattern::Path {
                subject: off_tech.into(),
                path: transitive_subclass_path_star(),
                object: framework_root.into(),
                graph_name: None,
            },
        )
    }

    #[cfg(feature = "sep-0006")]
    fn expensive_cartesian_subclass_path() -> GraphPattern {
        GraphPattern::Path {
            subject: Variable::new_unchecked("wide_left").into(),
            path: transitive_subclass_path_star(),
            object: Variable::new_unchecked("wide_right").into(),
            graph_name: None,
        }
    }

    #[cfg(feature = "sep-0006")]
    fn expensive_relaxed_subclass_lateral_shared_factor() -> GraphPattern {
        GraphPattern::join(
            attack_id_anchored_subclass_lateral_shared_factor(),
            expensive_cartesian_subclass_path(),
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        )
    }

    #[cfg(feature = "sep-0006")]
    fn expensive_singleton_subclass_lateral_shared_factor() -> GraphPattern {
        GraphPattern::join(
            singleton_anchored_subclass_lateral_factor(),
            expensive_cartesian_subclass_path(),
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        )
    }

    #[cfg(feature = "sep-0006")]
    fn raw_attack_id_anchored_subclass_shared_factor() -> GraphPattern {
        let off_tech = Variable::new_unchecked("s");
        let off_tech_id = Variable::new_unchecked("off_tech_id");
        let framework_root = Variable::new_unchecked("framework_root_iri");
        GraphPattern::join(
            GraphPattern::join(
                GraphPattern::extend(
                    GraphPattern::empty_singleton(),
                    off_tech_id.clone(),
                    Literal::new_simple_literal("T1546").into(),
                ),
                GraphPattern::QuadPattern {
                    subject: off_tech.clone().into(),
                    predicate: NamedNode::new_unchecked("http://example.com/attack-id").into(),
                    object: off_tech_id.into(),
                    graph_name: None,
                },
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![Variable::new_unchecked("off_tech_id")],
                },
            ),
            GraphPattern::join(
                GraphPattern::Path {
                    subject: off_tech.into(),
                    path: transitive_subclass_path_star(),
                    object: framework_root.into(),
                    graph_name: None,
                },
                expensive_cartesian_subclass_path(),
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        )
    }

    fn has_singleton_union_left_join(pattern: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::LeftJoin { left, right, .. } => {
                split_singleton_union_branches(right).is_some()
                    || has_singleton_union_left_join(left)
                    || has_singleton_union_left_join(right)
            }
            GraphPattern::Join { left, right, .. } | GraphPattern::Minus { left, right, .. } => {
                has_singleton_union_left_join(left) || has_singleton_union_left_join(right)
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                has_singleton_union_left_join(left) || has_singleton_union_left_join(right)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner, .. }
            | GraphPattern::Reduced { inner, .. }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. }
            | GraphPattern::Service { inner, .. } => has_singleton_union_left_join(inner),
            GraphPattern::Union { inner } => inner.iter().any(has_singleton_union_left_join),
            GraphPattern::Values { .. }
            | GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. } => false,
        }
    }

    fn has_singleton_union_inner_join(pattern: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::Join { left, right, .. } => {
                split_singleton_union_branches(left).is_some()
                    || split_singleton_union_branches(right).is_some()
                    || has_singleton_union_inner_join(left)
                    || has_singleton_union_inner_join(right)
            }
            GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Minus { left, right, .. } => {
                has_singleton_union_inner_join(left) || has_singleton_union_inner_join(right)
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                has_singleton_union_inner_join(left) || has_singleton_union_inner_join(right)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner, .. }
            | GraphPattern::Reduced { inner, .. }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. }
            | GraphPattern::Service { inner, .. } => has_singleton_union_inner_join(inner),
            GraphPattern::Union { inner } => inner.iter().any(has_singleton_union_inner_join),
            GraphPattern::Values { .. }
            | GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. } => false,
        }
    }

    fn has_union_inner_join_with_factor(pattern: &GraphPattern, factor: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::Join { left, right, .. } => {
                (matches!(left.as_ref(), GraphPattern::Union { .. }) && right.as_ref() == factor)
                    || (matches!(right.as_ref(), GraphPattern::Union { .. })
                        && left.as_ref() == factor)
                    || has_union_inner_join_with_factor(left, factor)
                    || has_union_inner_join_with_factor(right, factor)
            }
            GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Minus { left, right, .. } => {
                has_union_inner_join_with_factor(left, factor)
                    || has_union_inner_join_with_factor(right, factor)
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                has_union_inner_join_with_factor(left, factor)
                    || has_union_inner_join_with_factor(right, factor)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner, .. }
            | GraphPattern::Reduced { inner, .. }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. }
            | GraphPattern::Service { inner, .. } => {
                has_union_inner_join_with_factor(inner, factor)
            }
            GraphPattern::Union { inner } => inner
                .iter()
                .any(|branch| has_union_inner_join_with_factor(branch, factor)),
            GraphPattern::Values { .. }
            | GraphPattern::QuadPattern { .. }
            | GraphPattern::Path { .. }
            | GraphPattern::Graph { .. } => false,
        }
    }

    fn count_occurrences(pattern: &GraphPattern, target: &GraphPattern) -> usize {
        let self_count = usize::from(pattern == target);
        self_count
            + match pattern {
                GraphPattern::Join { left, right, .. }
                | GraphPattern::LeftJoin { left, right, .. }
                | GraphPattern::Minus { left, right, .. } => {
                    count_occurrences(left, target) + count_occurrences(right, target)
                }
                #[cfg(feature = "sep-0006")]
                GraphPattern::Lateral { left, right } => {
                    count_occurrences(left, target) + count_occurrences(right, target)
                }
                GraphPattern::Filter { inner, .. }
                | GraphPattern::Extend { inner, .. }
                | GraphPattern::OrderBy { inner, .. }
                | GraphPattern::Project { inner, .. }
                | GraphPattern::Distinct { inner, .. }
                | GraphPattern::Reduced { inner, .. }
                | GraphPattern::Slice { inner, .. }
                | GraphPattern::Group { inner, .. }
                | GraphPattern::Service { inner, .. } => count_occurrences(inner, target),
                GraphPattern::Union { inner } => inner
                    .iter()
                    .map(|branch| count_occurrences(branch, target))
                    .sum(),
                GraphPattern::Values { .. }
                | GraphPattern::QuadPattern { .. }
                | GraphPattern::Path { .. }
                | GraphPattern::Graph { .. } => 0,
            }
    }

    #[test]
    fn inner_join_with_singleton_union_branch_gets_rewritten() {
        let left = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let right = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            left.clone(),
            GraphPattern::union_all([GraphPattern::empty_singleton(), right]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            matches!(optimized, GraphPattern::Union { .. }),
            "join with `{{}} UNION {{...}}` should become a union of the left side and an inner join"
        );
        assert!(
            !has_singleton_union_inner_join(&optimized),
            "optimized plan should not keep a singleton-union inner join shape"
        );
    }

    #[test]
    fn true_left_join_with_singleton_union_branch_gets_rewritten() {
        let left = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let right = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::left_join(
            left.clone(),
            GraphPattern::union_all([GraphPattern::empty_singleton(), right]),
            true.into(),
            LeftJoinAlgorithm::HashBuildRightProbeLeft {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            matches!(optimized, GraphPattern::Union { .. }),
            "left join with `{{}} UNION {{...}}` should become a union of the left side and an inner join"
        );
        assert!(
            !has_singleton_union_left_join(&optimized),
            "optimized plan should not keep a singleton-union left join shape"
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn lateralized_subclass_inner_join_singleton_union_branch_is_not_duplicated() {
        let shared = expensive_relaxed_subclass_lateral_shared_factor();
        let anchored_factor = attack_id_anchored_subclass_lateral_shared_factor();
        let extra = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        assert!(
            should_avoid_singleton_union_duplication(&shared, &VariableTypes::default()),
            "test precondition: the shared factor should be expensive enough to skip singleton-union duplication",
        );
        let pattern = GraphPattern::join(
            shared.clone(),
            GraphPattern::union_all([GraphPattern::empty_singleton(), extra]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &anchored_factor),
            1,
            "lateralized anchored subclass factors should not be duplicated across singleton-union inner joins",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn lateralized_subclass_left_join_singleton_union_branch_is_not_duplicated() {
        let shared = expensive_relaxed_subclass_lateral_shared_factor();
        let anchored_factor = attack_id_anchored_subclass_lateral_shared_factor();
        let extra = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        assert!(
            should_avoid_singleton_union_duplication(&shared, &VariableTypes::default()),
            "test precondition: the shared factor should be expensive enough to skip singleton-union duplication",
        );
        let pattern = GraphPattern::left_join(
            shared.clone(),
            GraphPattern::union_all([GraphPattern::empty_singleton(), extra]),
            true.into(),
            LeftJoinAlgorithm::HashBuildRightProbeLeft {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &anchored_factor),
            1,
            "lateralized anchored subclass factors should not be duplicated across singleton-union left joins",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn flat_singleton_union_rewrite_preserves_non_singleton_branches() {
        let shared = expensive_relaxed_subclass_lateral_shared_factor();
        let anchored_factor = attack_id_anchored_subclass_lateral_shared_factor();
        let base = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("label").into(),
            graph_name: None,
        };
        let extra = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::join(
                shared,
                base.clone(),
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![Variable::new_unchecked("s")],
                },
            ),
            GraphPattern::union_all([GraphPattern::empty_singleton(), extra.clone()]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &anchored_factor),
            1,
            "the guarded anchored subclass factor should stay shared outside the rewritten singleton-UNION branch",
        );
        assert!(
            count_occurrences(&optimized, &extra) >= 1,
            "flattened singleton-union rewrites must preserve the non-singleton UNION branch",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn singleton_anchor_subclass_branch_is_still_duplicated_by_singleton_union_rewrite() {
        let shared = expensive_singleton_subclass_lateral_shared_factor();
        let anchored_factor = singleton_anchored_subclass_lateral_factor();
        let extra = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        assert!(
            !should_avoid_singleton_union_duplication(&shared, &VariableTypes::default()),
            "test precondition: singleton-anchored subclass laterals should not block singleton-union duplication",
        );
        let pattern = GraphPattern::join(
            shared,
            GraphPattern::union_all([GraphPattern::empty_singleton(), extra.clone()]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            matches!(optimized, GraphPattern::Union { .. }),
            "singleton-UNION rewrite should still fire when the subclass lateral only depends on a true singleton anchor",
        );
        assert!(
            count_occurrences(&optimized, &anchored_factor) >= 2,
            "large branches with only singleton-anchored subclass laterals should still be duplicable",
        );
    }

    #[cfg(feature = "sep-0006")]
    #[test]
    fn raw_attack_id_subclass_branch_is_not_duplicated_by_singleton_union_rewrite() {
        let shared = raw_attack_id_anchored_subclass_shared_factor();
        let anchored_factor = attack_id_anchored_subclass_lateral_shared_factor();
        let extra = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let reordered_shared = Optimizer::reorder_joins(shared.clone(), &VariableTypes::default());
        assert!(
            should_avoid_singleton_union_duplication(&reordered_shared, &VariableTypes::default()),
            "test precondition: the preview-reordered shared factor should expose the relaxed subclass lateral",
        );
        let pattern = GraphPattern::join(
            shared,
            GraphPattern::union_all([GraphPattern::empty_singleton(), extra.clone()]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![Variable::new_unchecked("s")],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &anchored_factor),
            1,
            "singleton-union rewrites should not duplicate raw branches that preview into the anchored subclass lateral shape",
        );
        assert!(
            count_occurrences(&optimized, &extra) >= 1,
            "singleton-union rewrites must preserve the non-singleton UNION branch for raw anchored subclasses too",
        );
    }

    #[test]
    fn small_factor_is_distributed_over_cartesian_union_branch() {
        let target = Variable::new_unchecked("target");
        let parent = Variable::new_unchecked("parent");
        let child = Variable::new_unchecked("child");

        let branch_a = GraphPattern::join(
            GraphPattern::Path {
                subject: parent.clone().into(),
                path: transitive_subclass_path_star(),
                object: NamedNode::new_unchecked("http://example.com/Artifact").into(),
                graph_name: None,
            },
            GraphPattern::Path {
                subject: target.clone().into(),
                path: transitive_subclass_path_star(),
                object: child.clone().into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        );
        let branch_b = GraphPattern::Path {
            subject: parent.clone().into(),
            path: transitive_subclass_path_star(),
            object: target.clone().into(),
            graph_name: None,
        };
        let factor = GraphPattern::QuadPattern {
            subject: child.clone().into(),
            predicate: NamedNode::new_unchecked(RDFS_SUBCLASS_OF_IRI).into(),
            object: parent.clone().into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::union_all([branch_a, branch_b]),
            factor.clone(),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![parent.clone()],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            !has_union_inner_join_with_factor(&optimized, &factor),
            "optimizer should not keep UNION directly joined with the bridge factor",
        );
        assert!(
            count_occurrences(&optimized, &factor) >= 2,
            "bridge factor should be pushed into UNION branches after distribution",
        );
    }

    #[test]
    fn flat_distribution_keeps_base_factors_shared() {
        let target = Variable::new_unchecked("target");
        let parent = Variable::new_unchecked("parent");
        let child = Variable::new_unchecked("child");
        let parent_label = Variable::new_unchecked("parent_label");

        let branch_a = GraphPattern::join(
            GraphPattern::Path {
                subject: parent.clone().into(),
                path: transitive_subclass_path_star(),
                object: NamedNode::new_unchecked("http://example.com/Artifact").into(),
                graph_name: None,
            },
            GraphPattern::Path {
                subject: target.clone().into(),
                path: transitive_subclass_path_star(),
                object: child.clone().into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        );
        let branch_b = GraphPattern::Path {
            subject: parent.clone().into(),
            path: transitive_subclass_path_star(),
            object: target.clone().into(),
            graph_name: None,
        };
        let factor = GraphPattern::QuadPattern {
            subject: child.clone().into(),
            predicate: NamedNode::new_unchecked(RDFS_SUBCLASS_OF_IRI).into(),
            object: parent.clone().into(),
            graph_name: None,
        };
        let base = GraphPattern::QuadPattern {
            subject: parent.clone().into(),
            predicate: NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label")
                .into(),
            object: parent_label.into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::join(
                base.clone(),
                factor.clone(),
                JoinAlgorithm::HashBuildLeftProbeRight {
                    keys: vec![parent.clone()],
                },
            ),
            GraphPattern::union_all([branch_a, branch_b]),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![parent.clone()],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            !has_union_inner_join_with_factor(&optimized, &factor),
            "optimizer should still push the bridge factor into UNION branches",
        );
        assert_eq!(
            count_occurrences(&optimized, &base),
            1,
            "base factors should stay shared instead of being duplicated per UNION branch",
        );
    }

    #[test]
    fn expensive_factor_is_not_distributed_over_union() {
        let x = Variable::new_unchecked("x");
        let a = Variable::new_unchecked("a");
        let u = Variable::new_unchecked("u");
        let v = Variable::new_unchecked("v");
        let y = Variable::new_unchecked("y");

        let branch_a = GraphPattern::join(
            GraphPattern::QuadPattern {
                subject: x.clone().into(),
                predicate: NamedNode::new_unchecked("http://example.com/p").into(),
                object: a.into(),
                graph_name: None,
            },
            GraphPattern::QuadPattern {
                subject: u.into(),
                predicate: NamedNode::new_unchecked("http://example.com/q").into(),
                object: v.into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        );
        let branch_b = GraphPattern::QuadPattern {
            subject: x.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/r").into(),
            object: Variable::new_unchecked("b").into(),
            graph_name: None,
        };
        let expensive_factor = GraphPattern::Path {
            subject: x.clone().into(),
            path: transitive_subclass_path_star(),
            object: y.into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::union_all([branch_a, branch_b]),
            expensive_factor.clone(),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![x.clone()],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            has_union_inner_join_with_factor(&optimized, &expensive_factor),
            "rewrite should stay off for factors above the configured cost threshold",
        );
        assert_eq!(
            count_occurrences(&optimized, &expensive_factor),
            1,
            "expensive factors must not be duplicated by UNION distribution",
        );
    }

    #[test]
    fn deep_unrelated_cartesian_join_does_not_trigger_distribution() {
        let x = Variable::new_unchecked("x");
        let left_deep = GraphPattern::QuadPattern {
            subject: x.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("p_obj").into(),
            graph_name: None,
        };
        let right_deep = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("u").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("q_obj").into(),
            graph_name: None,
        };
        let branch_with_deep_cartesian = GraphPattern::join(
            GraphPattern::join(
                left_deep,
                right_deep,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
            GraphPattern::QuadPattern {
                subject: x.clone().into(),
                predicate: NamedNode::new_unchecked("http://example.com/r").into(),
                object: Variable::new_unchecked("r_obj").into(),
                graph_name: None,
            },
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![x.clone()],
            },
        );
        let branch_without_cartesian = GraphPattern::QuadPattern {
            subject: x.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/s").into(),
            object: Variable::new_unchecked("s_obj").into(),
            graph_name: None,
        };
        let factor = GraphPattern::QuadPattern {
            subject: x.clone().into(),
            predicate: NamedNode::new_unchecked("http://example.com/f").into(),
            object: Variable::new_unchecked("f_obj").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            GraphPattern::union_all([branch_with_deep_cartesian, branch_without_cartesian]),
            factor.clone(),
            JoinAlgorithm::HashBuildLeftProbeRight {
                keys: vec![x.clone()],
            },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert!(
            has_union_inner_join_with_factor(&optimized, &factor),
            "distribution should not trigger from deep cartesian joins unrelated to the union root",
        );
    }

    #[test]
    fn duplicate_quad_factor_is_removed_from_join() {
        let duplicated = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let other = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/q").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            duplicated.clone(),
            GraphPattern::join(
                duplicated.clone(),
                other,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &duplicated),
            1,
            "optimizer should deduplicate repeated atomic join factors",
        );
    }

    #[test]
    fn duplicate_path_factor_is_removed_from_join() {
        let duplicated = GraphPattern::Path {
            subject: Variable::new_unchecked("s").into(),
            path: transitive_subclass_path_plus(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let other = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("o").into(),
            predicate: NamedNode::new_unchecked("http://example.com/p").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::join(
            duplicated.clone(),
            GraphPattern::join(
                duplicated.clone(),
                other,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
            JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
        );

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &duplicated),
            1,
            "optimizer should deduplicate repeated path join factors",
        );
    }

    #[test]
    fn common_atomic_factor_is_factored_out_of_union() {
        let common = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/common").into(),
            object: Variable::new_unchecked("o").into(),
            graph_name: None,
        };
        let left_only = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/left").into(),
            object: Variable::new_unchecked("x").into(),
            graph_name: None,
        };
        let right_only = GraphPattern::QuadPattern {
            subject: Variable::new_unchecked("s").into(),
            predicate: NamedNode::new_unchecked("http://example.com/right").into(),
            object: Variable::new_unchecked("y").into(),
            graph_name: None,
        };
        let pattern = GraphPattern::union_all([
            GraphPattern::join(
                common.clone(),
                left_only,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
            GraphPattern::join(
                common.clone(),
                right_only,
                JoinAlgorithm::HashBuildLeftProbeRight { keys: Vec::new() },
            ),
        ]);

        let optimized = Optimizer::optimize_graph_pattern(pattern);
        assert_eq!(
            count_occurrences(&optimized, &common),
            1,
            "optimizer should factor common atomic join factors out of UNION branches",
        );
    }
}

fn is_term_pattern_bound(pattern: &GroundTermPattern, input_types: &VariableTypes) -> bool {
    match pattern {
        GroundTermPattern::NamedNode(_) | GroundTermPattern::Literal(_) => true,
        GroundTermPattern::Variable(v) => !input_types.get(v).undef,
        #[cfg(feature = "sparql-12")]
        GroundTermPattern::Triple(t) => {
            is_term_pattern_bound(&t.subject, input_types)
                && is_named_node_pattern_bound(&t.predicate, input_types)
                && is_term_pattern_bound(&t.object, input_types)
        }
    }
}

fn is_rdfs_subclass_of_predicate(predicate: &NamedNodePattern) -> bool {
    matches!(predicate, NamedNodePattern::NamedNode(node) if node.as_str() == RDFS_SUBCLASS_OF_IRI)
}

fn is_named_node_pattern_bound(pattern: &NamedNodePattern, input_types: &VariableTypes) -> bool {
    match pattern {
        NamedNodePattern::NamedNode(_) => true,
        NamedNodePattern::Variable(v) => !input_types.get(v).undef,
    }
}
