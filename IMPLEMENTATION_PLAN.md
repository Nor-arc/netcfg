# netcfg: implementation plan for the next feature set

This document is the working brief for implementing the features agreed in design review. It is written to be handed to an implementing agent (Claude Code) as-is. Work through the phases in order; each phase must leave `cargo test` green, the four example template sets (`templates/{ios,nxos,eos,junos}`) loading with `netcfg validate`, and the README updated for anything user-visible.

## Repository orientation

```
netcfg/src/lexer.rs      indent and braces grammars -> Node tree; tokens borrow from input
netcfg/src/dialect.rs    declared dialects (builtins are declarations); grammar selection, rendering
netcfg/src/types.rs      Scalar trait, builtin catalog, UnionType, StructType, ListType, RegexType
netcfg/src/model.rs      template-file parser: dialect / type / model / template sections
netcfg/src/template.rs   template text -> TLine; shape inference; validation (pure functions)
netcfg/src/engine.rs     compile to patterns/slots; strict node-major parse; render; explain
netcfg/src/schema.rs     JSON Schema from model declarations
netcfg/src/main.rs       CLI (validate, parse, render, explain, schema, bench)
netcfg-py/               PyO3 bindings (maturin); exposes Engine, Parsed, explain, __version__/__build__
netcfg/tests/            smoke.rs, edge_cases.rs, junos.rs  (the acceptance suite)
templates/               example sets, one directory per dialect
```

Invariants that every change must preserve:

1. **Invertibility.** A template is a picture of the config with holes. No expressions, conditionals, filters, or regex in template lines. Every decision the template makes must be recoverable from the config text.
2. **Strict matching.** A config line that starts like a managed line but does not fully match is an error, never silently unmanaged. `@ignore` is the only opt-out.
3. **Three data states for a value field**: key missing (nothing written), `null` (the dialect's negated form), value. Flags are `true`/`false`, written only when different from the declared default.
4. **Everything declared, nothing coded**, except grammars and `Scalar` implementations.
5. **Errors name the file/model/field/line** and, where possible, say what to write instead.

Testing conventions: new behaviour gets tests in `netcfg/tests/` in the style of `edge_cases.rs` (must-fail with message fragment, must-be-unmanaged with path, must-parse-to, round trip). Any change to matching or rendering must keep the NX-OS and EOS example sets producing identical output for the existing tests. Python bindings get a smoke test run via `maturin build` + `pip install` + a short script.

---

## Phase 1: format rename and placeholder markers

### 1.1 Rename the format

The file extension `.ttp` collides with the TTP project (github.com/dmulyalin/ttp), from which the `{{ }}` placeholder syntax is borrowed. Rename:

- File extension `.ttp` -> `.nct`. The loader accepts both for one release, warning on `.ttp`.
- Keep the crate/CLI/package name `netcfg`.
- README: add a short "Relationship to TTP" section crediting TTP for the placeholder syntax and stating the differences (bidirectional, model-declared structure, strict matching, typed codecs, Rust engine).

Tests: loading a directory with `.nct` files works; a `.ttp` file loads with a deprecation warning on stderr.

### 1.2 Named templates

Today a `template` section pairs positionally with the `model` section before it. Make the pairing explicit by naming the model on the `template` line, without changing indentation:

```
model Neighbor
  peer: key ip
  remoteAs: asn?
  shutdown: flag

template Neighbor
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
    shutdown [[ shutdown ]]
```

Rules:
- `template NAME` may appear anywhere in the set after or before its model, in the same file or another file of the same set (4.1); loading resolves by name.
- Errors at load: a model without a template; a template naming an unknown model; two templates for one model (until versioned variants exist, 4.2, which will use `template NAME @selector`).
- A bare `template` (no name) is accepted for one release as "the immediately preceding model", with a deprecation warning naming file and line; `netcfg fmt` (new, small) rewrites files to the named form and, by default, places each template directly after its model. Remove the bare form in the following release.
- `fragment NAME` (2.4) uses the same shape: fields under `fragment`, lines under `template NAME`.
- Update every example template, the README, and the `model.rs` parser tests.

### 1.3 Remove redundant scalar types

`list(T)` and struct types make three builtins redundant. Remove them:

| Removed | Replacement |
|---|---|
| `names` | `list(string)` |
| `ints` | `list(int)` |
| `intpair` | a struct: `{{ keepalive: int }} {{ hold: int }}` (named parts instead of a two-element list) |

- The loader's "unknown type" error must suggest the replacement for these three names for one release.
- Update the NX-OS/EOS examples (`matchPrefixLists: list(string)?`, `setCommunity: list(string)?`, `timers` as a struct) and their tests. Note the behaviour change in the README: `list(int)` validates each element, so config with non-integers in those positions now fails strictly (which is the intended behaviour).
- Numbers are integers only. No float or decimal type in this plan; if one is needed later it will be a separate `decimal` type that preserves the written text, not a widening of `int`.

The builtin catalog after this item: `string`, `int`, `int(lo..hi)`, `list(T)`, `phrase`, `ipv4`, `ipv6`, `ip`, `cidr`, `ipv6cidr`, `prefix`, `asn`.

### 1.4 `<< model >>` placeholder for nested models

Currently `{{ neighbors }}` alone on a line denotes a collection. Introduce a third marker so the three placeholder kinds are visually distinct:

| Marker | Consumes | Field kinds |
|---|---|---|
| `{{ field }}` | tokens on the line | key, scalar, optional, defaulted |
| `[[ flag ]]` | nothing (line presence) | flag |
| `<< model >>` | whole statements/blocks at this level | `[Model]` collection, `Model?` singleton (1.5) |

Rules:
- `<< x >>` must be alone on its line (validator error otherwise).
- `{{ x }}` where `x` is a `[Model]` or `Model?` field is an error with the fix in the message; `<< x >>` where `x` is a scalar/flag is an error likewise (mirror the existing `[[ ]]` messages).
- Both grammars lex `<< ... >>` as one token (see how `[[ ]]` is handled in `lexer.rs::split_tokens` and `lex_braces`).
- Update all example templates, tests, `explain` output, and the README table.

### 1.5 Singleton nested models

Add field kind `Single`: `ospf: Ospf?` (optional) or `ospf: Ospf` (required) where `Ospf` is a keyed or unkeyed model. Semantics:

- Parse: at most one block/group of that model may match at this level; a second is `duplicate` error. Data is the record, or the key is missing when absent (required: error).
- Render: renders the single block.
- Works for block-shaped and flat-shaped models. For an unkeyed model (root shape) used as a singleton, identity is "the header line matches" -- allow root-shaped models to be used here only if their template has exactly one top-level line; otherwise error at load.
- `explain` shows `single Ospf` rows.

Tests: singleton block parse/render/round trip; duplicate error; required-missing error; `<< >>` for both collections and singletons; JSON Schema emits an object (not array) for singletons.

---

## Phase 2: declaration and template ergonomics

### 2.1 Field documentation

`remoteAs: asn?  # peer's AS; may be inherited from a peer template`

- A trailing `# ...` on a field line is its doc string (only outside template text).
- Carried into JSON Schema `description`, `explain` (one line under the field), and `skeleton` (2.5).
- Model-level doc: a comment block immediately preceding `model X` is the model's doc.

### 2.2 Comments inside template text

Template text may contain lines starting with `##` (two hashes, so it cannot collide with dialects using `#` as a config comment). They are dropped before lexing and never count as template lines. Document that single `#` inside template text is a literal.

### 2.3 Enum with data mapping

`type state = "up" -> true | "down" -> false`

- Each alternative may map a config literal to a data value (bool, int, or quoted string). Without `->` the literal maps to itself (existing behaviour).
- Parse yields the mapped value; render finds the literal whose mapped value equals the data value; ambiguous mappings (two literals -> same value) are a load error.
- Schema: `enum` of the mapped values.

### 2.4 Fragments

A reusable block of body lines without data of its own:

```
fragment CommonInterface
  description: phrase?
  mtu: int(576..9216) = 1500
  shutdown: flag

template CommonInterface
  description {{ description }}
  mtu {{ mtu }}
  shutdown [[ shutdown ]]
```

Used with `<< @CommonInterface >>` inside a model's template body. At load, the fragment's fields are merged into the including model (duplicate field name is an error) and its lines are spliced at that position. Fragments may not contain `<< >>` placeholders for models (keep it simple) and may not be nested. Same `model` / `template NAME` shape (1.2).

### 2.5 `netcfg skeleton --model X`

Emit example YAML for a model: every field present, typed placeholders (`<asn>`, `<ipv4>`, `<int 576..9216>`), `#` comments from docs and types, one example element per collection, `null`/missing explained once at the top. Also expose as `Engine.skeleton(model)` in Python.

### 2.6 `netcfg validate-data --model X data.yaml`

Validate a data file against a model without rendering. Reuse the render path's checks (unknown field, wrong type, missing required, struct shape) so messages are identical, but do not produce output; print every error, not just the first. Exit non-zero on failure.

---

## Phase 3: collections and lists

### 3.1 Positional collections

`entries: [AclEntry] ordered` -- elements are identified by position, not key. The element model may have no key fields. Parse: every matching block/line in order. Render: in data order. Duplicates are allowed. `netcfg diff` (Phase 5) treats a positional collection as replace-whole when anything differs.

### 3.2 Lists that stop before a literal

Inside struct types, a `list(T)` placeholder followed by literal or optional-literal tokens must stop consuming when the next token equals a following literal (or any literal of a following `""`-able union). Example: `type communityMatch = {{ names: list(string) }} {{ exact: "exact-match" | "" }}` must parse `A B exact-match` as `names=[A,B], exact=exact-match`. Implement by passing a stop-set to `ListType::parse`. Ambiguity (a list element that is also a stop word) resolves in favour of the stop word; document this.

---

## Phase 4: template sets and loading

### 4.1 Set manifest

Replace directory-as-set with an explicit manifest, keeping directory loading as the fallback:

```
set nxos
  dialect: nxos
  version: 2026.09.1
  include: ../common/routemap.nct
  files: *.nct            # default
```

- `Engine::load_set(path_to_manifest)`; `Engine::load_all(dir)` returns a map name -> Engine for every manifest found recursively.
- `include` paths are relative to the manifest; included files may define types and models; model name clashes are errors.
- `version` is recorded on the engine and stamped into `Parsed` (`Parsed.templates_version`) and exposed in Python.
- CLI: `--set path/to/set.nct` on every command; directory argument still accepted.

### 4.2 Template variants by OS version (design only)

Do not implement yet. Write a short design note in `docs/` covering `template nxos>=10` variants inside a model and how a set's `target` would select them. Stop there.

---

## Phase 5: change sets

### 5.1 Dialect `delete` property

`delete: no` (indent dialects, defaults to `negation`), `delete: delete` (Junos: a path prefix, `delete protocols bgp group EXT`). Braces dialects with `render: set` emit `delete <path>`; structured braces rendering has no deletion form and `diff` must error for it unless `render: set`.

### 5.2 `netcfg diff --model X running.cfg intent.yaml`

Produce an ordered list of commands that takes `running` to `intent`:

- Parse `running`; validate `intent` (2.6 semantics).
- Walk both by model: keyed collections match by key; positional collections replace-whole; singletons compare presence.
- Added block: render it in full. Removed block: `<delete> <header>`. Changed block: enter the block header, then per field: value changed -> render the line; value present -> absent (key missing) -> nothing unless `--explicit`; value -> `null` or `null` newly present -> negated form; flag changed -> positive or negated line per default rules.
- Output: config text in the dialect's syntax, ready to paste (`--format text`) or JSON list of ops (`--format json`).
- Unmanaged lines in `running` are never touched and are listed on stderr with `--show-unmanaged`.
- Library API: `Engine::diff(model, running: &Value, intent: &Value) -> ChangeSet`; Python `Engine.diff(model, running_dict, intent_dict)`.

Tests: add/remove/change neighbor on NX-OS and EOS; flag defaults; `null` handling; Junos `set`-style deletion; positional replace-whole; identical inputs produce an empty change set; applying the change set text to a rendered `running` then re-parsing equals `intent` (for indent dialects, where negated lines can be applied by re-parse semantics -- document the limits).

### 5.3 Render modes

`render --explicit`: write every flag and every defaulted field regardless of default. `--canonical` is the existing behaviour and the default.

### 5.4 Provenance

`Parsed` and the CLI JSON output carry `engine_version` and `templates_version` (from 4.1; `"unversioned"` for directory loads).

---

## Phase 6: authoring tools

### 6.1 `netcfg test`

Test files next to templates, `*.test.nct`:

```
test "neighbor with maximum-routes"
  model: EosNeighbor
  config:
    neighbor 1.1.1.1 remote-as 65431
    neighbor 1.1.1.1 maximum-routes 1200 warning-only
  expect:
    peer: 1.1.1.1
    remoteAs: 65431
    maximumRoutes: {limit: 1200, action: warning-only}
  roundtrip: true

test "unknown modifier fails"
  model: EosNeighbor
  config:
    neighbor 1.1.1.1 maximum-routes 1200 loudly
  fails: starts like a managed line

test "password is ignored"
  model: EosNeighbor
  config:
    neighbor 1.1.1.1 remote-as 1
    neighbor 1.1.1.1 password 7 abc
  unmanaged:
    - neighbor 1.1.1.1 password 7 abc
```

`expect` is YAML; comparison is exact (missing keys must be missing). `netcfg test <set>` runs all, prints pass/fail per test, exits non-zero on failure. Port a representative subset of `edge_cases.rs` into this format for the example sets, keeping the Rust tests as well.

### 6.2 `netcfg check --set S --golden dir/`

Parse every config under `dir/` with the set's device model (manifest property `root: Device`), fail on strict errors, and write the unmanaged report to `dir/.unmanaged/<config>.txt`; with `--compare`, diff against the committed report and fail on differences. This is the CI gate for template changes.

### 6.3 `netcfg suggest --model X config`

From the unmanaged report, group leaf lines by literal prefix (tokens up to the first token that varies across occurrences), count occurrences, and print for each group a proposed template line and field declaration:

```
router bgp > neighbor * > bfd             (412x)   bfd [[ bfd ]]                      bfd: flag
router bgp > neighbor * > password 3 *    (390x)   password 3 {{ password }}          password: string?
```

Heuristics: constant suffix -> flag; one varying trailing token -> `string?` (or `int?` if all numeric, `ipv4?`/`ipv6?` if all addresses); several varying tokens -> `phrase?`. Mark groups already covered by `@ignore`. No automatic edits.

### 6.4 `netcfg lint --set S`

Report: template lines shadowed by an earlier line with the same literal prefix; flags whose default the goldens (if given) never contradict; fields never observed in goldens; literal prefixes shared between models at the same level (ambiguous claims); `@ignore` prefixes that never match. Warnings, not errors, unless `--strict`.

### 6.5 Cross-field references (declaration only + validate-data)

`routeMapIn: string? references RouteMapEntry.name` -- at `validate-data` and before `render`/`diff`, check that the value exists among the referenced model's key values in the same data tree. Schema ignores it. Keep the syntax minimal: `references Model.field` on optional/required scalar fields.

---

## Phase 7 (later, design first): folded fields, language server

- Folded fields (several lines <-> one value, e.g. `send-community` + `send-community extended` vs `both`): write a design note; do not implement without review.
- Language server: `tower-lsp` over `template.rs`/`model.rs` providing diagnostics, completion of field names and types inside `{{ }}`/`[[ ]]`/`<< >>`, hover with `explain` rows; plus a TextMate grammar. Separate crate `netcfg-lsp`. Scope after Phases 1-6.

---

## Definition of done, per phase

- `cargo test` green; new tests cover each bullet above.
- `netcfg validate` passes on every example set; example templates use the new syntax where applicable.
- README updated; `explain` output updated where the feature is visible there.
- Python bindings updated for any new engine API, wheel builds with `maturin build --release`, and the smoke script runs.
- Commit per phase (or per numbered item for large phases) with a message naming the item.

Decisions that were taken and should not be relitigated during implementation: no logic in templates; `null` is the negated form; flags carry the device default; `[[ ]]` and `<< >>` markers; templates name their model (`template Neighbor`) with no extra indentation; integers only, no floats; `list(T)` replaces the ad-hoc list types; dialects are declarations; grammars and `Scalar` implementations are code; the JVM/Scala prototype is reference only.
