# oxixml-regex

The regular-expression engine shared across the
[OxiXML](https://github.com/cool-japan/oxixml) workspace — the COOLJAPAN Pure-Rust foundation for
XML and RDF processing. `std`-only, depending only on `oxixml-unicode` for Unicode property data;
the whole engine (parser, a linear-time Pike VM, and a budgeted back-tracking interpreter for
patterns with back-references) is implemented here from the two W3C specifications, with no
dependency on the `regex` crate.

A `Regex` can be built in either of two dialects a conforming XML stack must implement:

* `Regex::xsd` — the XML Schema `pattern` facet: implicitly anchored to the whole string, no
  flags, no back-references, but with range-subtraction character classes (`[a-z-[aeiou]]`) and
  the XML-name-aware `\i`/`\c`/`\p{...}`/`\p{Is...}` escapes.
* `Regex::xpath` — the XPath/XQuery `fn:matches` flavour: unanchored, with `^`/`$` anchors, the
  `s`/`m`/`i`/`x`/`q` flags, reluctant quantifiers, non-capturing groups, and back-references.

## Install

```toml
[dependencies]
oxixml-regex = "0.1.2"
```

## Usage

```rust
use oxixml_regex::Regex;

// XSD: implicitly anchored to the whole string.
let postcode = Regex::xsd("[0-9]{5}(-[0-9]{4})?")?;
assert!(postcode.is_match("12345"));
assert!(postcode.is_match("12345-6789"));
assert!(!postcode.is_match("12345-678")); // must match the whole string

// XPath: unanchored search with the case-insensitive flag.
let word = Regex::xpath("hello", "i")?;
assert!(word.is_match("Well, HELLO there"));

// fn:replace with capture references.
let swap = Regex::xpath("(\\w+)\\s+(\\w+)", "")?;
assert_eq!(swap.replace("John Smith", "$2, $1")?, "Smith, John");
# Ok::<(), oxixml_regex::RegexError>(())
```

## What it replaces

A pure-Rust alternative to pairing the `regex` crate (no back-references, no XSD range
subtraction) with `fancy-regex` (no XSD dialect, no `fn:analyze-string`-style per-group match
positions) — one engine covering both W3C grammars.

## Conformance

Regression-tested against cases audited from the [QT3 test suite](https://github.com/qt4cg/qt3tests)
(all 266 `fn:matches` cases expected to raise `FORX0002` in `fn/matches.re.xml`) and the
[W3C XSD Test Suite](https://github.com/w3c/xsdtests) (every `xs:pattern` facet across the 2577
schemas that use exactly one) for the specific defects those audits turned up — a regression suite
built from real audits, not a claim of exhaustive, on-every-build conformance against either suite.

## License

Licensed under the Apache License, Version 2.0
([LICENSE](https://github.com/cool-japan/oxixml/blob/main/LICENSE) or
<http://www.apache.org/licenses/LICENSE-2.0>).
