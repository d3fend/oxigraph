use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

#[test]
fn optimizer_preserves_not_exists_scope_across_subquery_projection() {
    let store = Store::new().unwrap();
    load_scope_regression_data(&store);

    let query = r#"
        PREFIX ex: <http://example.com/>
        SELECT ?s
        WHERE {
          { SELECT ?s WHERE { ?s ex:p ?o . } }
          FILTER NOT EXISTS { GRAPH ex:g1 { ?s ex:p ?o . } }
        }
        ORDER BY ?s
    "#;

    let optimized_rows = run_subjects(&store, query, false);
    let unoptimized_rows = run_subjects(&store, query, true);

    assert_eq!(
        optimized_rows, unoptimized_rows,
        "optimizer must not leak projected-away bindings into EXISTS/NOT EXISTS correlation",
    );
    assert_eq!(
        unoptimized_rows,
        vec![
            "<http://example.com/b>".to_owned(),
            "<http://example.com/c>".to_owned(),
        ]
    );
}

#[test]
fn optimizer_preserves_exists_scope_across_subquery_projection() {
    let store = Store::new().unwrap();
    load_scope_regression_data(&store);

    let query = r#"
        PREFIX ex: <http://example.com/>
        SELECT ?s
        WHERE {
          { SELECT ?s WHERE { ?s ex:p ?o . } }
          FILTER EXISTS { GRAPH ex:g1 { ?s ex:p ?o . } }
        }
        ORDER BY ?s
    "#;

    let optimized_rows = run_subjects(&store, query, false);
    let unoptimized_rows = run_subjects(&store, query, true);

    assert_eq!(
        optimized_rows, unoptimized_rows,
        "optimizer must not leak projected-away bindings into EXISTS/NOT EXISTS correlation",
    );
    assert_eq!(unoptimized_rows, vec!["<http://example.com/a>".to_owned()]);
}

fn load_scope_regression_data(store: &Store) {
    store
        .update(
            r#"
            PREFIX ex: <http://example.com/>
            INSERT DATA {
              ex:a ex:p 1 .
              ex:b ex:p 2 .
              ex:c ex:p 3 .
              GRAPH ex:g1 {
                ex:a ex:p 999 .
              }
            }
            "#,
        )
        .unwrap();
}

fn run_subjects(store: &Store, query: &str, without_optimizations: bool) -> Vec<String> {
    let mut evaluator = SparqlEvaluator::new();
    if without_optimizations {
        evaluator = evaluator.without_optimizations();
    }
    let prepared = evaluator.parse_query(query).unwrap();
    let results = prepared.on_store(store).execute().unwrap();
    let QueryResults::Solutions(solutions) = results else {
        panic!("expected SELECT query results");
    };
    solutions
        .map(|solution| solution.unwrap().get("s").unwrap().to_string())
        .collect()
}
