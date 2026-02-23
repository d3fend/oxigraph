use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

#[test]
fn optimizer_preserves_implicit_empty_group_row_for_count() {
    let store = Store::new().unwrap();
    let query = "SELECT (COUNT(*) AS ?v) WHERE { FILTER(false) }";

    let optimized = run_optional_literal(&store, query, false);
    let unoptimized = run_optional_literal(&store, query, true);

    assert_eq!(
        optimized, unoptimized,
        "optimizer must preserve the implicit empty group row for aggregate queries"
    );
    assert_eq!(unoptimized, vec![Some("0".to_owned())]);
}

#[test]
fn optimizer_preserves_implicit_empty_group_row_for_min() {
    let store = Store::new().unwrap();
    let query = "SELECT (MIN(?x) AS ?v) WHERE { FILTER(false) }";

    let optimized = run_optional_literal(&store, query, false);
    let unoptimized = run_optional_literal(&store, query, true);

    assert_eq!(
        optimized, unoptimized,
        "optimizer must preserve the implicit empty group row for aggregate queries"
    );
    assert_eq!(unoptimized, vec![None]);
}

#[test]
fn optimizer_keeps_group_by_empty_result_empty() {
    let store = Store::new().unwrap();
    let query = "SELECT ?x (COUNT(*) AS ?v) WHERE { FILTER(false) } GROUP BY ?x";

    let optimized = run_optional_literal(&store, query, false);
    let unoptimized = run_optional_literal(&store, query, true);

    assert_eq!(optimized, unoptimized);
    assert!(unoptimized.is_empty());
}

fn run_optional_literal(
    store: &Store,
    query: &str,
    without_optimizations: bool,
) -> Vec<Option<String>> {
    let mut evaluator = SparqlEvaluator::new();
    if without_optimizations {
        evaluator = evaluator.without_optimizations();
    }
    let prepared = evaluator.parse_query(query).unwrap();
    let results = prepared.on_store(store).execute().unwrap();
    let QueryResults::Solutions(solutions) = results else {
        panic!("expected SELECT results");
    };
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            solution.get("v").map(|term| match term {
                Term::Literal(literal) => literal.value().to_owned(),
                _ => panic!("expected literal aggregate output"),
            })
        })
        .collect()
}
