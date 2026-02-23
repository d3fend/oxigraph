use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

fn iri(value: &str) -> NamedNode {
    NamedNode::new(value).unwrap()
}

fn insert_edge(store: &Store, subject: &str, predicate: &str, object: &str) {
    let quad = Quad::new(
        iri(subject),
        iri(predicate),
        iri(object),
        GraphName::DefaultGraph,
    );
    store.insert(&quad).unwrap();
}

fn query_bindings(store: &Store, query: &str, variable: &str) -> Vec<String> {
    let prepared = SparqlEvaluator::new().parse_query(query).unwrap();
    let results = prepared.on_store(store).execute().unwrap();
    match results {
        QueryResults::Solutions(solutions) => solutions
            .map(|solution| solution.unwrap().get(variable).unwrap().to_string())
            .collect(),
        _ => panic!("expected SELECT query results"),
    }
}

fn query_ask(store: &Store, query: &str) -> bool {
    let prepared = SparqlEvaluator::new().parse_query(query).unwrap();
    let results = prepared.on_store(store).execute().unwrap();
    match results {
        QueryResults::Boolean(value) => value,
        _ => panic!("expected ASK query results"),
    }
}

fn insert_edge_literal(store: &Store, subject: &str, predicate: &str, object: &str) {
    let quad = Quad::new(
        iri(subject),
        iri(predicate),
        Literal::from(object),
        GraphName::DefaultGraph,
    );
    store.insert(&quad).unwrap();
}

#[test]
fn property_path_zero_or_more_includes_start_node() {
    let store = Store::new().unwrap();
    insert_edge(
        &store,
        "http://example.com/a",
        "http://example.com/p",
        "http://example.com/b",
    );

    let rows = query_bindings(
        &store,
        "PREFIX ex: <http://example.com/> SELECT ?o WHERE { ex:a ex:p* ?o } ORDER BY ?o",
        "o",
    );
    assert_eq!(
        rows,
        vec![
            "<http://example.com/a>".to_owned(),
            "<http://example.com/b>".to_owned(),
        ]
    );
}

#[test]
fn property_path_zero_or_more_reflexive_on_empty_graph() {
    let store = Store::new().unwrap();
    assert!(query_ask(
        &store,
        "PREFIX ex: <http://example.com/> ASK { ex:x ex:p* ex:x }",
    ));
}

#[test]
fn property_path_zero_or_more_returns_bound_term_on_empty_graph() {
    let store = Store::new().unwrap();
    let rows = query_bindings(
        &store,
        "PREFIX ex: <http://example.com/> SELECT ?o WHERE { ex:x ex:p* ?o } ORDER BY ?o",
        "o",
    );
    assert_eq!(rows, vec!["<http://example.com/x>".to_owned()]);
}

#[test]
fn property_path_one_or_more_excludes_start_node() {
    let store = Store::new().unwrap();
    insert_edge(
        &store,
        "http://example.com/a",
        "http://example.com/p",
        "http://example.com/b",
    );

    let rows = query_bindings(
        &store,
        "PREFIX ex: <http://example.com/> SELECT ?o WHERE { ex:a ex:p+ ?o } ORDER BY ?o",
        "o",
    );
    assert_eq!(rows, vec!["<http://example.com/b>".to_owned()]);
}

#[test]
fn property_path_reverse_one_or_more_finds_predecessors() {
    let store = Store::new().unwrap();
    insert_edge(
        &store,
        "http://example.com/a",
        "http://example.com/p",
        "http://example.com/b",
    );
    insert_edge(
        &store,
        "http://example.com/b",
        "http://example.com/p",
        "http://example.com/c",
    );

    let rows = query_bindings(
        &store,
        "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ex:c ^ex:p+ ?s } ORDER BY ?s",
        "s",
    );
    assert_eq!(
        rows,
        vec![
            "<http://example.com/a>".to_owned(),
            "<http://example.com/b>".to_owned(),
        ]
    );
}

#[test]
fn property_path_cycle_is_finite_and_returns_expected_nodes() {
    let store = Store::new().unwrap();
    insert_edge(
        &store,
        "http://example.com/a",
        "http://example.com/p",
        "http://example.com/b",
    );
    insert_edge(
        &store,
        "http://example.com/b",
        "http://example.com/p",
        "http://example.com/a",
    );

    let rows = query_bindings(
        &store,
        "PREFIX ex: <http://example.com/> SELECT ?o WHERE { ex:a ex:p+ ?o } ORDER BY ?o",
        "o",
    );
    assert_eq!(
        rows,
        vec![
            "<http://example.com/a>".to_owned(),
            "<http://example.com/b>".to_owned(),
        ]
    );
}

#[test]
fn property_path_zero_or_more_allows_literal_reflexive_binding_when_literal_is_in_graph() {
    let store = Store::new().unwrap();
    insert_edge_literal(&store, "http://example.com/s", "http://example.com/p", "x");

    let rows = query_bindings(
        &store,
        "SELECT ?s ?o WHERE { VALUES (?s ?o) { (\"x\" \"x\") } ?s <http://example.com/p>* ?o }",
        "s",
    );
    assert_eq!(rows, vec!["\"x\"".to_owned()]);
}

#[test]
fn property_path_zero_or_more_literal_reflexive_ask_is_true_when_literal_is_in_graph() {
    let store = Store::new().unwrap();
    insert_edge_literal(&store, "http://example.com/s", "http://example.com/p", "x");

    assert!(query_ask(
        &store,
        "ASK { \"x\" <http://example.com/p>* \"x\" }",
    ));
}
