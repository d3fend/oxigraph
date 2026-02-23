use spargebra::SparqlParser;

#[test]
fn ask_with_group_by_is_accepted() {
    SparqlParser::new()
        .parse_query("ASK WHERE { ?s ?p ?o } GROUP BY ?s")
        .unwrap();
}

#[test]
fn construct_with_group_by_is_accepted() {
    SparqlParser::new()
        .parse_query("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o } GROUP BY ?s ?p ?o")
        .unwrap();
}

#[test]
fn describe_star_with_group_by_is_accepted() {
    SparqlParser::new()
        .parse_query("DESCRIBE * WHERE { ?s ?p ?o } GROUP BY ?s")
        .unwrap();
}

#[test]
fn select_star_with_group_by_still_fails() {
    assert!(
        SparqlParser::new()
            .parse_query("SELECT * WHERE { ?s ?p ?o } GROUP BY ?s")
            .is_err()
    );
}
