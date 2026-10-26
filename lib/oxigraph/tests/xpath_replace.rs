//! Raw RDF oracles for the XPath replacement language shared by expression routes.

use oxigraph::model::{Dataset, Literal, Term};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};

#[expect(clippy::panic)]
fn evaluate(query: &str, without_optimizations: bool) -> Vec<Option<Term>> {
    let dataset = Dataset::new();
    let mut evaluator = SparqlEvaluator::new();
    if without_optimizations {
        evaluator = evaluator.without_optimizations();
    }
    let QueryResults::Solutions(results) = evaluator
        .parse_query(query)
        .unwrap()
        .on_queryable_dataset(&dataset)
        .execute()
        .unwrap()
    else {
        panic!("expected SELECT results");
    };
    results
        .map(|result| result.unwrap().get("result").cloned())
        .collect()
}

#[test]
fn replace_static_and_dynamic_preserve_values_and_expression_errors() {
    let simple = |value| Some(Term::Literal(Literal::new_simple_literal(value)));
    for (arguments, expected) in [
        ([r#""abc""#, r#""b""#, r#""d""#, r#""""#], simple("adc")),
        ([r#""abc""#, r#""b""#, r#""\\$""#, r#""""#], simple("a$c")),
        ([r#""abc""#, r#""b""#, r#""\\\\""#, r#""""#], simple("a\\c")),
        (
            [r#""a""#, r#""(a)""#, r#""$010|$099|$000|$2""#, r#""""#],
            simple("a0|9|a|"),
        ),
        (
            [r#""b""#, r#""(a)?b""#, r#""[$1][$0]""#, r#""""#],
            simple("[][b]"),
        ),
        ([r#""abc""#, r#""a*""#, r#""X""#, r#""""#], None),
        ([r#""nomatch""#, r#""b""#, r#""$""#, r#""""#], None),
        ([r#""nomatch""#, r#""b""#, r#""\\q""#, r#""""#], None),
        (
            [r#""abc""#, r#""b""#, r#""$0\\$""#, r#""q""#],
            simple("a$0\\$c"),
        ),
        ([r#""a b""#, r#"" ""#, r#""_""#, r#""qx""#], simple("a_b")),
        ([r#""a""#, r#"" ""#, r#""_""#, r#""qx""#], simple("a")),
        (
            [r#""é猫é""#, r#""(é)""#, r#""🐈$1""#, r#""""#],
            simple("🐈é猫🐈é"),
        ),
        (
            [r#""abc"@en"#, r#""b""#, r#""d""#, r#""""#],
            Some(
                Literal::new_language_tagged_literal("adc", "en")
                    .unwrap()
                    .into(),
            ),
        ),
        ([r#""abc""#, r#""b"@en"#, r#""d""#, r#""""#], None),
        ([r#""abc""#, r#""b""#, r#""d"@en"#, r#""""#], None),
        ([r#""abc""#, r#""b""#, r#""d""#, r#""qz""#], None),
        (["123", r#""b""#, r#""d""#, r#""""#], None),
        ([r#""abc""#, r#""b""#, r#""d""#, r#""q"@en"#], None),
    ] {
        for dynamic in [false, true] {
            let expression = if dynamic {
                "REPLACE(?text, ?pattern, ?replacement, ?flags)".to_owned()
            } else {
                format!("REPLACE({})", arguments.join(", "))
            };
            let input = if dynamic {
                format!(
                    "VALUES (?text ?pattern ?replacement ?flags) {{ ({}) }}",
                    arguments.join(" ")
                )
            } else {
                String::new()
            };
            for without_optimizations in [false, true] {
                assert_eq!(
                    evaluate(
                        &format!("SELECT ({expression} AS ?result) WHERE {{ {input} }}"),
                        without_optimizations
                    ),
                    vec![expected.clone()],
                    "{expression}, {input}, without_optimizations={without_optimizations}"
                );
                // SRL SET's error behavior is BIND followed by BOUND filtering.
                assert_eq!(
                    evaluate(
                        &format!(
                            "SELECT ?result WHERE {{ {input} BIND({expression} AS ?result) FILTER(BOUND(?result)) }}"
                        ),
                        without_optimizations
                    ),
                    expected.clone().map(Some).into_iter().collect::<Vec<_>>()
                );
                // A filter evaluates the same expression, including errors.
                assert_eq!(
                    evaluate(
                        &format!(
                            "SELECT ?result WHERE {{ {input} FILTER(isLITERAL({expression})) }}"
                        ),
                        without_optimizations
                    )
                    .len(),
                    usize::from(expected.is_some())
                );
            }
        }
    }
}

#[cfg(feature = "rdf-12")]
#[test]
fn replace_preserves_directional_language() {
    use oxigraph::model::BaseDirection;

    let expected: Term =
        Literal::new_directional_language_tagged_literal("adc", "en", BaseDirection::Rtl)
            .unwrap()
            .into();
    for without_optimizations in [false, true] {
        assert_eq!(
            evaluate(
                r#"SELECT (REPLACE("abc"@en--rtl, "b", "d") AS ?result) WHERE {}"#,
                without_optimizations
            ),
            vec![Some(expected.clone())]
        );
    }
}
