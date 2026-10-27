use oxrdf::{Dataset, Literal, NamedNode};
use spareval::{QueryEvaluationError, QueryEvaluator, QueryResults, QuerySolution};
use spargebra::SparqlParser;

// This bounded compiled-program failure is deterministic; it does not depend on
// memory pressure, a long input string, or pathological matching time.
const LARGE_PATTERN: &str = r#""a{200001}""#;

fn evaluator(optimized: bool) -> QueryEvaluator {
    if optimized {
        QueryEvaluator::new()
    } else {
        QueryEvaluator::new().without_optimizations()
    }
}

fn solutions(query: &str, optimized: bool) -> Result<Vec<QuerySolution>, QueryEvaluationError> {
    let query = SparqlParser::new()
        .parse_query(query)
        .unwrap_or_else(|error| panic!("Invalid test query: {error}\n{query}"));
    let dataset = Dataset::new();
    let QueryResults::Solutions(solutions) =
        evaluator(optimized).prepare(&query).execute(&dataset)?
    else {
        panic!("Expected SELECT results")
    };
    solutions.collect()
}

fn assert_resource_limit(error: QueryEvaluationError) {
    assert!(
        matches!(error, QueryEvaluationError::ResourceLimit { .. }),
        "Expected typed compilation limit, got {error:?}"
    );
}

#[test]
fn static_and_dynamic_regex_and_replace_propagate_through_filter_and_bind() {
    for optimized in [false, true] {
        for expression in [
            format!(r#"REGEX("a", {LARGE_PATTERN})"#),
            format!(r#"REPLACE("a", {LARGE_PATTERN}, "x")"#),
            r#"REGEX("a", ?pattern)"#.into(),
            r#"REPLACE("a", ?pattern, "x")"#.into(),
        ] {
            for position in [
                format!("FILTER({expression})"),
                format!("BIND({expression} AS ?result)"),
            ] {
                let query = format!(
                    "SELECT * WHERE {{ VALUES ?pattern {{ {LARGE_PATTERN} }} {position} }}"
                );
                assert_resource_limit(solutions(&query, optimized).unwrap_err());
            }
        }
    }
}

#[test]
fn nested_functions_and_evaluated_control_flow_preserve_fatal_errors() {
    let regex = format!(r#"REGEX("a", {LARGE_PATTERN})"#);
    let replace = format!(r#"REPLACE("a", {LARGE_PATTERN}, "x")"#);
    for optimized in [false, true] {
        for expression in [
            format!("COALESCE({regex}, false)"),
            format!("IF(true, {regex}, false)"),
            format!("IF(false, false, {regex})"),
            format!("true && {regex}"),
            format!("false || {regex}"),
            format!("!{regex}"),
            format!("STRLEN({replace}) + 1"),
            format!("MD5({replace})"),
            format!("<http://www.w3.org/2001/XMLSchema#string>({replace})"),
            format!(r#"sameTerm({replace}, "a")"#),
        ] {
            let query = format!("SELECT ({expression} AS ?result) WHERE {{}}");
            assert_resource_limit(solutions(&query, optimized).unwrap_err());
        }
    }
}

#[test]
fn unselected_control_flow_does_not_compile_a_failing_pattern() {
    let regex = format!(r#"REGEX("a", {LARGE_PATTERN})"#);
    for optimized in [false, true] {
        for (expression, expected) in [
            (format!("COALESCE(true, {regex})"), true),
            (format!("IF(true, true, {regex})"), true),
            (format!("IF(false, {regex}, true)"), true),
            (format!("false && {regex}"), false),
            (format!("true || {regex}"), true),
        ] {
            let rows = solutions(
                &format!("SELECT ({expression} AS ?result) WHERE {{}}"),
                optimized,
            )
            .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("result"), Some(&Literal::from(expected).into()));
        }
    }
}

#[test]
fn ordinary_pattern_and_replacement_errors_keep_sparql_option_semantics() {
    for optimized in [false, true] {
        for expression in [
            r#"REGEX("a", "[")"#,
            r#"REPLACE("a", "[", "x")"#,
            r#"REPLACE("a", "a", "$x")"#,
            r#"REPLACE("a", "a*", "x")"#,
            r#"REGEX("a", "a", "z")"#,
        ] {
            assert!(
                solutions(
                    &format!("SELECT * WHERE {{ FILTER({expression}) }}"),
                    optimized
                )
                .unwrap()
                .is_empty()
            );
            let rows = solutions(
                &format!("SELECT ({expression} AS ?result) WHERE {{}}"),
                optimized,
            )
            .unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].get("result").is_none());
            let rows = solutions(
                &format!("SELECT (COALESCE({expression}, true) AS ?result) WHERE {{}}"),
                optimized,
            )
            .unwrap();
            assert_eq!(rows[0].get("result"), Some(&Literal::from(true).into()));
        }
    }
}

#[test]
fn exists_minus_optional_sort_and_aggregate_do_not_hide_resource_limits() {
    let regex = format!(r#"REGEX(STR(?key), {LARGE_PATTERN})"#);
    for optimized in [false, true] {
        for query in [
            format!(
                "SELECT (EXISTS {{ FILTER({regex}) }} AS ?result) \
                 WHERE {{ VALUES ?key {{ 1 }} }}"
            ),
            format!(
                "SELECT * WHERE {{ VALUES ?key {{ 1 }} \
                 FILTER EXISTS {{ FILTER EXISTS {{ FILTER({regex}) }} }} }}"
            ),
            format!(
                "SELECT * WHERE {{ VALUES ?key {{ 1 2 }} \
                 MINUS {{ VALUES ?key {{ 1 2 }} FILTER({regex}) }} }}"
            ),
            format!(
                "SELECT * WHERE {{ VALUES ?key {{ 1 2 }} \
                 OPTIONAL {{ VALUES ?key {{ 1 2 }} FILTER({regex}) }} }}"
            ),
            format!(
                "SELECT ?key WHERE {{ VALUES ?key {{ 1 2 }} }} \
                 ORDER BY (IF(?key = 1, true, {regex}))"
            ),
            format!(
                "SELECT (COUNT(IF(?key = 1, true, {regex})) AS ?result) \
                 WHERE {{ VALUES ?key {{ 1 2 }} }}"
            ),
            format!("SELECT (SAMPLE({regex}) AS ?result) WHERE {{ VALUES ?key {{ 1 2 }} }}"),
            format!(
                "SELECT ?group (COUNT(*) AS ?result) WHERE {{ VALUES ?key {{ 1 2 }} }} \
                 GROUP BY (IF(?key = 1, true, {regex}) AS ?group)"
            ),
            format!(
                r#"SELECT * WHERE {{ VALUES ?pattern {{ {LARGE_PATTERN} "a" }}
                   BIND(REGEX("a", ?pattern) AS ?result) }} OFFSET 1"#
            ),
        ] {
            assert_resource_limit(
                solutions(&query, optimized)
                    .expect_err(&format!("Expected a resource failure: {query}")),
            );
        }
    }
}

#[test]
fn ask_returns_an_encountered_resource_error_before_a_later_valid_row() {
    for optimized in [false, true] {
        let query = SparqlParser::new()
            .parse_query(&format!(
                r#"ASK {{ VALUES ?pattern {{ {LARGE_PATTERN} "a" }}
                    FILTER(REGEX("a", ?pattern)) }}"#
            ))
            .unwrap();
        let dataset = Dataset::new();
        assert_resource_limit(
            evaluator(optimized)
                .prepare(&query)
                .execute(&dataset)
                .err()
                .expect("ASK must not turn an encountered resource error into true"),
        );
    }
}

#[test]
fn successful_limit_and_ask_short_circuit_unevaluated_later_rows() {
    for optimized in [false, true] {
        let pattern = format!(
            r#"{{ VALUES ?pattern {{ "a" {LARGE_PATTERN} }} FILTER(REGEX("a", ?pattern)) }}"#
        );
        assert_eq!(
            solutions(&format!("SELECT * WHERE {pattern} LIMIT 1"), optimized)
                .unwrap()
                .len(),
            1
        );
        let query = SparqlParser::new()
            .parse_query(&format!("ASK {pattern}"))
            .unwrap();
        let dataset = Dataset::new();
        assert!(matches!(
            evaluator(optimized).prepare(&query).execute(&dataset),
            Ok(QueryResults::Boolean(true))
        ));
    }
}

#[test]
fn later_stream_error_remains_visible_after_a_successful_row() {
    for optimized in [false, true] {
        let query = SparqlParser::new()
            .parse_query(&format!(
                r#"SELECT ?result WHERE {{ VALUES ?pattern {{ "a" {LARGE_PATTERN} }}
                    BIND(REGEX("a", ?pattern) AS ?result) }}"#
            ))
            .unwrap();
        let dataset = Dataset::new();
        let QueryResults::Solutions(mut rows) = evaluator(optimized)
            .prepare(&query)
            .execute(&dataset)
            .unwrap()
        else {
            panic!("Expected SELECT results")
        };
        assert_eq!(
            rows.next().unwrap().unwrap().get("result"),
            Some(&Literal::from(true).into())
        );
        assert_resource_limit(rows.next().unwrap().unwrap_err());
    }
}

#[test]
fn custom_function_does_not_receive_failed_arguments() {
    let query = SparqlParser::new()
        .parse_query(&format!(
            r#"SELECT (<urn:identity>(REPLACE("a", {LARGE_PATTERN}, "x")) AS ?result) WHERE {{}}"#
        ))
        .unwrap();
    let dataset = Dataset::new();
    let evaluator = QueryEvaluator::new()
        .with_custom_function(NamedNode::new_unchecked("urn:identity"), |_| {
            panic!("A fatal argument must prevent the custom function call")
        });
    let QueryResults::Solutions(mut rows) = evaluator.prepare(&query).execute(&dataset).unwrap()
    else {
        panic!("Expected SELECT results")
    };
    assert_resource_limit(rows.next().unwrap().unwrap_err());
}

#[test]
fn precancelled_regex_and_replace_preserve_cancellation() {
    for optimized in [false, true] {
        for expression in [r#"REGEX("a", "a")"#, r#"REPLACE("a", "a", "b")"#] {
            let query = SparqlParser::new()
                .parse_query(&format!("SELECT ({expression} AS ?result) WHERE {{}}"))
                .unwrap();
            let dataset = Dataset::new();
            let token = spareval::CancellationToken::new();
            token.cancel();
            let evaluator = evaluator(optimized).with_cancellation_token(token);
            let error = match evaluator.prepare(&query).execute(&dataset) {
                Err(error) => error,
                Ok(QueryResults::Solutions(mut rows)) => rows.next().unwrap().unwrap_err(),
                _ => panic!("Expected SELECT results"),
            };
            assert!(matches!(error, QueryEvaluationError::Cancelled));
        }
    }
}

struct FatalService(bool);

impl spareval::ServiceHandler for FatalService {
    type Error = QueryEvaluationError;
    fn handle(
        &self,
        _: &spargebra::algebra::GraphPattern,
        _: Option<&oxiri::Iri<String>>,
    ) -> Result<spareval::QuerySolutionIter<'static>, Self::Error> {
        Err(if self.0 {
            QueryEvaluationError::Cancelled
        } else {
            QueryEvaluationError::ResourceLimit {
                stage: "fixture",
                limit: 1,
            }
        })
    }
}

#[test]
fn service_silent_preserves_resource_errors_and_cancellation() {
    for cancelled in [false, true] {
        for optimized in [false, true] {
            let query = SparqlParser::new()
                .parse_query("SELECT * WHERE { SERVICE SILENT <urn:fatal> {} }")
                .unwrap();
            let dataset = Dataset::new();
            let evaluator = evaluator(optimized).with_service_handler(
                NamedNode::new_unchecked("urn:fatal"),
                FatalService(cancelled),
            );
            let error = match evaluator.prepare(&query).execute(&dataset) {
                Err(error) => error,
                Ok(QueryResults::Solutions(mut rows)) => rows.next().unwrap().unwrap_err(),
                _ => panic!("Expected SELECT results"),
            };
            if cancelled {
                assert!(matches!(error, QueryEvaluationError::Cancelled));
            } else {
                assert_resource_limit(error);
            }
        }
    }
}

#[test]
fn xpath_semantics_flow_through_static_and_dynamic_expressions() {
    for optimized in [false, true] {
        for expression in [
            r#"REGEX("bcdf", "[a-z-[aeiou]]+")"#,
            r#"REGEX("aa", "(a)\\1")"#,
            r#"REGEX("K", "k", "i")"#,
            r#"REGEX("bcdf", ?pattern)"#,
        ] {
            let rows = solutions(&format!(r#"SELECT ({expression} AS ?result) WHERE {{ VALUES ?pattern {{ "[a-z-[aeiou]]+" }} }}"#), optimized).unwrap();
            assert_eq!(rows[0].get("result"), Some(&Literal::from(true).into()));
        }
    }
}

#[test]
fn backtracking_discriminators_preserve_static_and_dynamic_expression_results() {
    for optimized in [false, true] {
        for (pattern, flags, text, expected) in [
            (r"^(a|ab)\1$", "", "abab", Some(true)),
            (r"^(a*)a\1$", "", "aaaaa", Some(true)),
            (r"^(a*)a\1$", "", "aaaa", Some(false)),
            (r"^(a*?)a\1$", "", "aaaaa", Some(true)),
            (r"^((a)b|a)\2$", "", "a", Some(true)),
            ("^a$", "iimmsx", "A", Some(true)),
            ("a", "qz", "a", None),
        ] {
            let pattern = Literal::from(pattern);
            let flags = Literal::from(flags);
            let text = Literal::from(text);
            for expression in [
                format!("REGEX({text}, {pattern}, {flags})"),
                "REGEX(?text, ?pattern, ?flags)".into(),
            ] {
                let rows = solutions(
                    &format!(
                        "SELECT ({expression} AS ?result) WHERE {{ \
                         VALUES (?text ?pattern ?flags) {{ ({text} {pattern} {flags}) }} }}"
                    ),
                    optimized,
                )
                .unwrap();
                let expected: Option<oxrdf::Term> =
                    expected.map(|value| Literal::from(value).into());
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("result"), expected.as_ref(), "{expression}");
            }
        }
        for (pattern, text, replacement, expected) in [
            ("(a)|(ab)", "ab", "[$1][$2]", Some("[a][]b")),
            (r"(a)?\1", "z", "X", None),
        ] {
            let pattern = Literal::from(pattern);
            let text = Literal::from(text);
            let replacement = Literal::from(replacement);
            for expression in [
                format!("REPLACE({text}, {pattern}, {replacement})"),
                "REPLACE(?text, ?pattern, ?replacement)".into(),
            ] {
                let rows = solutions(
                    &format!(
                        "SELECT ({expression} AS ?result) WHERE {{ \
                         VALUES (?text ?pattern ?replacement) {{ ({text} {pattern} {replacement}) }} }}"
                    ),
                    optimized,
                )
                .unwrap();
                let expected: Option<oxrdf::Term> =
                    expected.map(|value| Literal::from(value).into());
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("result"), expected.as_ref(), "{expression}");
            }
        }
    }
}
