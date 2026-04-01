use crate::algebra::{
    Expression, GraphPattern, JoinAlgorithm, LeftJoinAlgorithm, MinusAlgorithm, OrderExpression,
};
use crate::type_inference::{
    VariableType, VariableTypes, infer_expression_type, infer_graph_pattern_types,
};
use oxrdf::Variable;
use spargebra::algebra::PropertyPathExpression;
use spargebra::term::{GroundTermPattern, NamedNodePattern};
use std::cmp::{max, min};
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
const DEFAULT_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE: usize = 10_000;
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

fn subclass_for_loop_max_left_size() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        env::var("OXIGRAPH_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SUBCLASS_FOR_LOOP_MAX_LEFT_SIZE)
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
        let pattern = Self::normalize_pattern(pattern, &VariableTypes::default());
        let pattern = Self::reorder_joins(pattern, &VariableTypes::default());
        Self::push_filters(pattern, Vec::new(), &VariableTypes::default())
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
                if let Some((singleton_union_index, non_singleton_branches)) =
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
                    let mut base_factors = to_reorder;
                    base_factors.remove(singleton_union_index);
                    let base_join = Self::reorder_joins(
                        join_all_factors(base_factors.clone())
                            .expect("join flattening should keep at least one factor"),
                        input_types,
                    );
                    if non_singleton_branches.is_empty() {
                        return base_join;
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
                                let output_size = estimate_graph_pattern_size(&output, input_types);
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
                            let output_size = estimate_graph_pattern_size(&output, input_types);
                            output = if is_fit_for_for_loop_join(
                                &next,
                                input_types,
                                &output_types,
                                output_size,
                            ) {
                                GraphPattern::lateral(output, next)
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
                if expression.effective_boolean_value() == Some(true) {
                    if let Some((singleton_branch_count, non_singleton_branches)) =
                        split_singleton_union_branches(&right)
                    {
                        if singleton_branch_count == 1 {
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
                let right_types = infer_graph_pattern_types(&right, input_types.clone());
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
            } => GraphPattern::extend(
                Self::reorder_joins(*inner, input_types),
                variable,
                expression,
            ),
            GraphPattern::Filter { inner, expression } => {
                GraphPattern::filter(Self::reorder_joins(*inner, input_types), expression)
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
            if entry_estimated_size > transitive_lateral_max_left_size() {
                return false;
            }
            let entry_start_bound = is_term_pattern_bound(subject, entry_types);
            let entry_end_bound = is_term_pattern_bound(object, entry_types);
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
        GraphPattern::Join { .. }
        | GraphPattern::Minus { .. }
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
                max(
                    estimate_path_size(start_bound, p, end_bound)
                        .saturating_mul(open_transitive_path_factor()),
                    open_transitive_path_min_cost(),
                )
            } else {
                open_transitive_path_full_scan_cost()
            }
        }
        PropertyPathExpression::OneOrMore(p) => {
            if start_bound && end_bound {
                1
            } else if start_bound || end_bound {
                max(
                    estimate_path_size(start_bound, p, end_bound)
                        .saturating_mul(open_transitive_path_factor()),
                    open_transitive_path_min_cost(),
                )
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
    use oxrdf::NamedNode;

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

    #[test]
    fn open_transitive_path_cost_is_strongly_penalized() {
        let star = transitive_subclass_path_star();
        let plus = transitive_subclass_path_plus();

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
            path: transitive_subclass_path_plus(),
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
