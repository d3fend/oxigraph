# D3FEND XPath substrate

This source import replaces spareval's Rust-regex-compatible subset for SPARQL
`REGEX` and `REPLACE`. The evaluator explicitly selects XPath 3.1 syntax with
XML Schema 1.1 character classes. It preserves its lazy static-pattern cache
and ordinary SPARQL expression errors, while resource exhaustion and
cancellation propagate as fatal query errors.

## Provenance and changes

`UPSTREAM.json` records the exact crates.io `oxixml-regex` 0.1.2 archive,
SHA-256, actual archive VCS revision, and pre-edit file hashes. The archive is
from revision `6aaaf48f928364aa7dbef60dc181ebb2d10847b2`; it differs from the
older 0.1.2 tag used during initial assessment. Local version
`0.1.2-d3fend.1` identifies this modified source. Apache-2.0 licensing is
retained; the generated Unicode-derived tables also carry Unicode-3.0.
`UNICODE-CASE.md` records exact Unicode input hashes, licensing and generation.
The only substrate dependency is pinned `oxixml-unicode = "=0.1.2"`.

Changes include typed work/storage/output limits for compilation and both VMs;
iterative optional-repeat emission; correct oversized-repeat rejection rather
than silently compiling an impossible match; back-reference digit parsing;
class-aware `x` whitespace handling; strict property-name grammar; multiline
end anchors; full Unicode upper/lower case-variant pairs; cached block ranges;
and bounded XPath replacement-template parsing and output construction.

## Resource contract

Default limits are 1 MiB pattern/flags, 64 MiB input, 64 MiB output,
64 MiB cumulative requested allocation volume, 10 million charged operations,
200 parser nesting levels, 200,000 compiled instructions and 4,096 captures.
`Limits` allows substrate callers to choose smaller or larger allowances,
except the parser's hard nesting ceiling of 200. Allocation accounting covers
requested vector/string/class/capture storage, not allocator overhead or RSS.
Compiled-program ownership and pre-existing input storage are outside the
subsequent match operation's cumulative allocation counter.

Compilation has its own allowance. Matching, an entire `find_iter`, replacement
(including the empty-match probe, all searches and output), or tokenization
shares one allowance across that operation. This is **not a query-wide total**:
each evaluated expression on each row receives an operation allowance. A
query-wide aggregate CPU or memory allowance is not implemented here.

The existing query cancellation token is checked during regex compilation,
matching and replacement. Fatal errors survive ASK, join paths and SERVICE
SILENT. Cancellation is cooperative; synchronous browser calls cannot process
a main-thread event while executing. Worker execution and browser latency
validation remain the host's responsibility. The inherited bool `is_match`
convenience cannot expose errors; spareval uses only fallible APIs.

## Validation status

Source implementation and focused tests are provided. No native/WASM build,
independent full conformance result, browser responsiveness result, or speed
claim is implied by this import. Root validation must run the existing upstream
suites, new semantic/resource/Unicode cases, spareval propagation tests, and
native/WASM builds before the full XPath completion blocker can close.

Normative references: [XPath 3.1 syntax](https://www.w3.org/TR/xpath-functions-31/#regex-syntax),
[flags](https://www.w3.org/TR/xpath-functions-31/#flags),
[replacement](https://www.w3.org/TR/xpath-functions-31/#func-replace), and
[XML Schema 1.1 classes](https://www.w3.org/TR/xmlschema11-2/#regexs).
