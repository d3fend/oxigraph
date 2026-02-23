use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

const ROW_COUNT: usize = 256;

#[test]
fn exists_with_rand_is_not_reused_for_every_row() {
    let store = Store::new().unwrap();
    let query = volatile_exists_query(ROW_COUNT);

    for without_optimizations in [false, true] {
        let mut evaluator = SparqlEvaluator::new();
        if without_optimizations {
            evaluator = evaluator.without_optimizations();
        }
        let prepared = evaluator.parse_query(&query).unwrap();
        let results = prepared.on_store(&store).execute().unwrap();
        let QueryResults::Solutions(solutions) = results else {
            panic!("expected SELECT results");
        };
        let count = solutions.count();
        assert!(
            count > 0 && count < ROW_COUNT,
            "expected a partial match count for volatile EXISTS, got {count} (without_optimizations={without_optimizations})"
        );
    }
}

fn volatile_exists_query(row_count: usize) -> String {
    let values = (1..=row_count)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "SELECT ?i WHERE {{ VALUES ?i {{ {values} }} FILTER EXISTS {{ FILTER(RAND() < 0.5) }} }}"
    )
}
