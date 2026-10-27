# XPath character equivalence

The `i` flag follows XPath 3.1 §5.6.2: two single characters match when their
**full lowercase strings are equal OR their full uppercase strings are equal**.
This is a pairwise relation, not Unicode case folding and not a transitive
equivalence closure. It applies to literal characters, character ranges, and
each corresponding character in a back-reference. Property and shorthand
escapes are unchanged. Negation and subtraction apply after the member rules.

For example, `ﬅ` and `ﬆ` have the same full uppercase mapping `ST`, so they are
case variants. `İ` and `i` are not: their full lowercase mappings differ.
`θ` matches both `ϑ` and `ϴ`, but `ϑ` and `ϴ` do not match one another. A single
`ß` never consumes two `s` characters.

The Unicode snapshot is **17.0.0**, from the exact dependency
`oxixml-unicode = "=0.1.2"`. Rust's toolchain-dependent Unicode tables are not
used for character equivalence or property membership.
The generated Unicode data carries the [Unicode License V3](LICENSE-UNICODE),
copied from <https://www.unicode.org/license.txt> on 2026-10-26.

| Input | SHA-256 |
| --- | --- |
| oxixml-unicode 0.1.2 crate archive, 191,027 bytes | `6f4fe405939d195999b5e957038d6c66e14ee766c94a98b08309afa04548bdb6` |
| `src/tables/simple_case.rs` | `5a33efb26d4dbb561e5b3f95d34dfe09bb66d1f8e63221e4f1ae348b1574def5` |
| `src/tables/special_case.rs` | `40b80a06f925d3b1c9a6251d47976e1f956ded6fbe4fb30da13b5cc2ba5d32f5` |
| Reference API `src/case.rs` | `200a8ae0cb82d2ec4f74424afa1feae6050e154356d11e67b6b19c65986b8e83` |
| Block table `src/tables/blocks.rs` | `f14c34cf90ccb387b3803cde5fb9efbeb728ea4a366f3e82302d959d66bb2f82` |
| Generated `src/case_variants.rs`, 82,785 bytes | `45132a4da9202bc9f215d1cd281183fb1d018a05ca8276a4c2d4aa57315a9a84` |

The Unicode crate records VCS revision
`6aaaf48f928364aa7dbef60dc181ebb2d10847b2` and package directory
`support/oxixml-unicode`. Its generated input tables come from UnicodeData and
the unconditional entries in SpecialCasing. For a single-character input,
locale-independent `Final_Sigma` cannot apply because there is no preceding
cased character. Locale-specific tailoring does not apply to XPath's default
case operations.

Regenerate offline from the unpacked, pinned dependency:

```sh
python3 tools/generate_case_variants.py /path/to/oxixml-unicode-0.1.2
python3 tools/generate_case_variants.py /path/to/oxixml-unicode-0.1.2 --check
```

The generator verifies input SHA-256 hashes before reading the tables. It
combines simple mappings with full special mappings, constructs inverse images
of each full lowercase and uppercase output independently, and emits their
pairwise union without transitive closure. Every nonidentity pair must involve
a mapped character; including each one-character output also covers its
possible identity-mapped partner. The result contains **3,092 nonidentity
pairs**, with at most **3 other variants per character**. Runtime lookup uses a
sorted static table and makes no string allocations.

The unit oracle scans every Unicode scalar through the dependency's full
string-mapping APIs and independently reconstructs the complete inverse-image
relation. Public tests exercise both directions through literals, singleton
classes, ranges, and back-references, plus nontransitivity, negation,
subtraction, property escapes, and XML-name boundaries. Resource-bounded
matching charges lookup and candidate traversal work; class cloning charges
each node and item before allocation. Unicode block names are resolved to
scalar ranges once during parsing, including empty surrogate-only blocks.
Block resolution uses no temporary normalized strings and precharges its table
scan using the pinned table's 346 rows and maximum 48-byte name. The generator
checks those bounds and the block-table hash. Legacy-name and surrogate-name
comparisons are charged separately.

Normative references:

- [XPath 3.1 regular expression flags](https://www.w3.org/TR/xpath-functions-31/#flags)
- [XPath full uppercase mapping](https://www.w3.org/TR/xpath-functions-31/#func-upper-case)
- [XPath full lowercase mapping](https://www.w3.org/TR/xpath-functions-31/#func-lower-case)
- [Unicode 17.0.0 SpecialCasing](https://www.unicode.org/Public/17.0.0/ucd/SpecialCasing.txt)
- [Unicode 17.0.0 UnicodeData](https://www.unicode.org/Public/17.0.0/ucd/UnicodeData.txt)
