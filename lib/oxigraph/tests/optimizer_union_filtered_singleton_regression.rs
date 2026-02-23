use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

#[test]
fn optimizer_keeps_union_filtered_singleton_multiplicity() {
    let store = Store::new().unwrap();
    let query = r#"
        SELECT (COUNT(*) AS ?c)
        WHERE {
          {
            {}
            UNION
            { FILTER EXISTS { VALUES ?x { 1 } } }
          }
        }
    "#;

    let optimized_count = run_count(&store, query, false);
    let unoptimized_count = run_count(&store, query, true);

    assert_eq!(
        optimized_count, unoptimized_count,
        "optimizer and non-optimizer execution must preserve multiset multiplicity"
    );
    assert_eq!(unoptimized_count, 2);
}

fn run_count(store: &Store, query: &str, without_optimizations: bool) -> i64 {
    let mut evaluator = SparqlEvaluator::new();
    if without_optimizations {
        evaluator = evaluator.without_optimizations();
    }
    let prepared = evaluator.parse_query(query).unwrap();
    let results = prepared.on_store(store).execute().unwrap();
    let QueryResults::Solutions(mut solutions) = results else {
        panic!("expected SELECT results");
    };
    let solution = solutions.next().unwrap().unwrap();
    let count_term = solution.get("c").unwrap();
    match count_term {
        oxigraph::model::Term::Literal(literal) => literal.value().parse::<i64>().unwrap(),
        _ => panic!("expected literal count"),
    }
}
