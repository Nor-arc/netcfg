# netcfg (Rust)

Bidirectional network-config templates as a Rust core, a CLI, and a Python package.
Authors write `.nct` files (model declarations plus config-shaped template text); the
same template parses a running config into data and renders data back into config.
No Scala, no code generation: templates and dialects are loaded and validated at runtime,
and the data side is plain dicts / JSON / YAML with a generated JSON Schema.

```text
type action = permit | deny

model RouteMapEntry
  name: key string
  action: action
  seq: key int
  description: phrase?
  matchPrefixLists: list(string)?
  setLocalPref: int?

template RouteMapEntry
  route-map {{ name }} {{ action }} {{ seq }}
    description {{ description }}
    match ip address prefix-list {{ matchPrefixLists }}
    set local-preference {{ setLocalPref }}
```

## Relationship to TTP

The `{{ placeholder }}` syntax is borrowed from [TTP](https://github.com/dmulyalin/ttp)
(Template Text Parser), with thanks. netcfg differs in what it is for:

- **Bidirectional.** The same template parses config into data and renders data into config.
- **Model-declared structure.** Fields, types, keys, collections and defaults are declared in
  a `model`; the template only places them. There are no expressions, filters or
  conditionals in template lines, so every decision is recoverable from the config text.
- **Strict matching.** A line that starts like a managed line but doesn't fully match is an
  error, never silently dropped; `@ignore` is the only opt-out.
- **Typed codecs.** Values are parsed and written by typed codecs (`ipv4`, `asn`, `cidr`,
  structs, unions), and the data has a generated JSON Schema.
- **Rust engine** with a CLI and Python bindings.

Files used the `.ttp` extension until 0.4; that collides with TTP, so the extension is now
`.nct`. `.ttp` files still load for one release, with a deprecation warning.

## The `.nct` format

A set is described by a manifest (see [Template sets](#template-sets)), or is simply a
directory of `.nct` files. Each file holds `dialect`, `type`, `model` (and
`fragment`) declarations and `template NAME` sections. A template names the model it
belongs to and may sit anywhere in the set; `netcfg fmt` places each one directly after
its model. (A bare `template` still pairs with the model above it, with a deprecation
warning, for one release; `netcfg fmt` rewrites it.)

Templates use three placeholder markers, one per kind of field:

| Marker | Consumes | Field kinds |
|---|---|---|
| `{{ field }}` | tokens on the line | key, required value, optional, defaulted |
| `[[ flag ]]` | nothing (the line's presence) | flag |
| `<< model >>` | whole statements/blocks at this level; alone on its line | `[Model]` collection, `Model` / `Model?` singleton |

| Declaration | Meaning |
|---|---|
| `f: key T` | `f` is part of the block's identity; several keys form a composite key. Keys go on the header line (blocks) or on every line (flat groups). |
| `f: T` | Required value. May also appear on the header line (`route-map {{ name }} {{ action }} {{ seq }}`). |
| `f: T = default` | Required, with a default used when the line is absent; the line is omitted when rendering the default. |
| `f: T?` | The line is optional. |
| `f: T? references Model.key` | A cross-field reference: the value must be a `key` value of some `Model` record in the same data tree (`routeMapIn: string? references RouteMapEntry.name`). Checked by `validate-data`, `render` and `diff`, not by `parse` (devices accept dangling references) and not in the JSON Schema. Data that cannot contain `Model` is not checked. |
| `f: flag` / `f: flag = true` (written `[[ f ]]` in the template) | Presence of a literal line; `<negation> <line>` is `false`. Set the default to the device's default so negated lines render exactly when needed. A flag whose template literals start with the negation word (`no ip address {{ cleared }}`) is matched literally and only has that spelling. |
| `f: [Model]` | A keyed collection; `<< f >>` alone on a line stands for all of its blocks/lines. |
| `f: [Model] ordered` | A positional collection: elements are identified by position, not key. Every matching block/line is an element, in config order; rendering follows data order; duplicates are allowed. The element model may have no key (then its template must have exactly one top-level line, like an ACL entry `{{ action }} {{ match }}`). Flat groups can't be positional. |
| `f: Model?` / `f: Model` | A singleton nested model (optional / required): at most one block or group of `Model` at this level, a second is a `duplicate` error. The data is a record, or the key is missing when absent. A model without keys can be a singleton if its template has exactly one top-level line (`snmp-server` with nested lines); that line is its identity. |
| `type name = /regex/` | One-token type validated by a regex. |
| `type name = "a" \| "b"` | Enumeration of literal tokens (quoted). |
| `type name = asn \| "auto"` | Union: alternatives tried in order; bare names are types, quoted words are literals. |
| `type state = "up" -> true \| "down" -> false` | Enum with data mapping: a literal maps to a data value (`true`/`false`, an integer, or a `"quoted string"`); without `->` it maps to itself. Rendering picks the literal whose value matches; two literals mapping to the same value is a load error. The JSON Schema `enum` lists the data values. |
| `type name = string \| ""` | An empty literal matches nothing at the end of the line: a value that may be present without a value (`neighbor X group` vs `neighbor X group CORE`, data `""` vs `"CORE"`). Must be the last placeholder. |
| `type name = {{ limit: int }} {{ action: "warning-only" \| "" }}` | Struct type: a value's own little template. Data is a record (`{limit: 1200, action: warning-only}`); sub-fields whose type allows `""` may sit anywhere and are omitted when absent. Placeholders may use inline unions. |
| `f: {{ limit: int }} {{ action: "warning-only" \| "" }}?` | Anonymous struct on a field, for one-off shapes; `type` is for reused ones. |
| `list(T)` | Rest-of-line list of `T` (`prependAsPath: list(prependItem)?`). `T` must be a one-token type. Inside a struct type, a list stops before a literal that can follow it: in `{{ names: list(string) }} {{ exact: "exact-match" \| "" }}`, `A B exact-match` is `names: [A, B], exact: exact-match`. A token that could be either an element or that literal is taken as the literal. |
| constant line (`exit-address-family`, `neighbor {{ peer }} activate`) | A line with no placeholders (in a flat group: only the key ones) and no nested lines. It must be present, carries no data, and is always rendered in template position. A literal-only line *with* nested lines is a container, as before. |
| `@ignore word word *` | Explicit opt-out: lines starting with these words are reported as unmanaged, never errors. |
| `fragment Name` + `<< @Name >>` | A reusable run of body lines with its own fields and no identity (see below). |

Builtin types: `string`, `int`, `int(lo..hi)`, `list(T)`, `phrase` (free text to the end of the line), `ipv4`, `ipv6`, `ip` (either), `cidr` (dialect-dependent: `addr/len` on NX-OS/EOS, `addr mask` on IOS; the value is always `addr/len`), `ipv6cidr`, `prefix` (either), `asn` (asplain or asdot in, asplain out). Numbers are integers only. Domain types with real parsing logic are added in Rust by implementing the `Scalar` trait.

`names`, `ints` and `intpair` were removed in 0.4: write `list(string)`, `list(int)`, and a struct with named parts (`timers: {{ keepalive: int }} {{ hold: int }}?`, data `{keepalive: 10, hold: 30}` instead of `[10, 30]`). The loader names the replacement if an old type is used. Note that `list(int)` validates every element, so config with a non-integer in such a position is now a strict error rather than accepted.

### Documentation and comments

```text
# A BGP neighbor block.                  <- a comment block directly above `model` is its doc
model Neighbor
  peer: key ip
  remoteAs: asn?  # peer's AS; may be inherited from a peer template

template Neighbor
  ## Lines starting with `##` are template comments; they are dropped before lexing.
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
```

Docs appear in the JSON Schema (`description`), in `netcfg explain` (a `#` line under the
field) and in `netcfg skeleton`. Inside template text a single `#` is an ordinary literal
token (`description # {{ d }}` matches `description # to core`); only `##` starts a
comment, so dialects that use `#` in config are unaffected. Outside template text, `#`
starts a comment as before.

### Fragments

```text
fragment PeerSession
  description: phrase?
  updateSource: string?
  ebgpMultihop: int(2..255)?

template PeerSession
  description {{ description }}
  update-source {{ updateSource }}
  ebgp-multihop {{ ebgpMultihop }}

template Neighbor
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
    << @PeerSession >>
```

At load the fragment's lines are spliced in at `<< @PeerSession >>` and its fields are merged
into the including model, placed after the fields bound above the include so the data keeps
template order. A field name that the model already has is an error. Fragments have no keys,
may not contain nested models (`<< >>`) and may not include other fragments. Errors inside a
fragment name it: `fragment PeerSession line 8 ...`. The NX-OS example shares `PeerSession`
between `Neighbor` and `PeerTemplate`.

Template shapes are inferred: one header line with nested lines is a **block**; several sibling lines that all carry the key are a **flat group** (EOS/IOS `neighbor X …` lines); a model without keys is a **root** document.

## Template sets

A set manifest is a `.nct` file with a `set` section:

```text
set nxos
  dialect: nxos
  version: 2026.09.1
  include: ../common/routemap.nct
  files: *.nct            # the default
  root: Device            # the device model (used by `netcfg check`)
```

Paths are relative to the manifest. `files` and `include` take comma-separated globs (`*`,
`?`, `**`); `include` may repeat and each pattern must match a file. Included files may
define types and models; a model defined twice is an error. The manifest itself and
`*.test.nct` files are never template files. `dialect` names a builtin, or the dialect the
set's files declare (the two must agree).

`version` is recorded on the engine and stamped into every parse result
(`Parsed.templates_version`, `"unversioned"` for directory loads) next to
`engine_version`. `Engine::load_set(manifest)` loads one set; `Engine::load_all(dir)` loads
every manifest found under `dir`, by set name. A directory that contains a manifest loads as
that set, so `templates/nxos` and `templates/nxos/set.nct` are the same thing; a directory
without one still loads every `.nct` file under it. The example sets under `templates/`
each have a manifest, and NX-OS and EOS share `templates/common/routemap.nct` through
`include`.

Versioned template variants (`template NAME @nxos>=10.2`) are designed but not implemented:
see [docs/template-variants.md](docs/template-variants.md).

## Dialects

A dialect is declared, not coded, in a `dialect` section (conventionally `dialect.nct` next to the templates):

```text
dialect junos
  grammar: braces        # indent | braces
  indent: 4
  cidr: slash            # type conventions: slash | masked
  render: structured     # braces only: structured | set

dialect iosxe
  extends: ios           # builtins: cisco, ios, nxos, eos, junos
  skip: end, Building configuration, Current configuration, Load for
```

Properties: `grammar`, `indent`, `comments`, `skip`, `block-separator`, `end-marker`, `negation` (the prefix that negates a flag line, `no`), `delete` (the prefix that removes a statement or block in a change set; defaults to `negation` on indent dialects, `delete` on Junos, where it is a path prefix: `delete protocols bgp group EXT`), `render`, and type knobs such as `cidr`. Templates are lexed with the dialect's grammar, so a Junos template is written in Junos syntax:

```text
template JunosDevice
  system {
      host-name {{ hostname }};
  }
  protocols {
      bgp {
          << groups >>
      }
  }
```

Only two things are code: a new **grammar** (how text becomes a statement tree; `indent` and `braces` exist, `set`-style input would be a third) and a new **type implementation** with its own parsing logic (`Scalar` trait). Everything that merely varies per platform is a declaration.

## Spellings and `null` (how `no` works)

With `negation: no` declared in the dialect, every value line has a negated form that the template never needs to write:

| Config | Data |
|---|---|
| nothing about the field | key missing |
| `no ip address` | `address: null` |
| `ip address 10.1.1.1/32` | `address: "10.1.1.1/32"` |

Parsing `no <the line's literals>` yields `null` for an optional field (and the default for a defaulted one); `null` in intent data renders the negated form, which is also the command that clears the setting on the device. A missing key writes nothing. Flags follow the same idea with `true`/`false`: `no shutdown` is `false`, and a flag is written when its value differs from its declared default, so declare the *device's* default (`shutdown: flag = true` on platforms that shut interfaces by default). Writing `no shutdown [[ shutdown ]]` in a template is allowed for readability and changes nothing. `netcfg explain` prints this table for any model, and the JSON Schema marks optional fields nullable when the dialect has a negation word.

## Change sets (`netcfg diff`)

```
netcfg diff templates/nxos running.cfg intent.yaml --model Device            # config text to paste
netcfg diff templates/nxos running.cfg intent.yaml --model Device --format json
```

`diff` parses the running config, validates the intent (every error, as `validate-data`),
and walks both by model:

- keyed collections match elements by key: an added element is rendered in full, a removed
  one is `<delete> <header>` (`no neighbor 10.1.0.2`; for an EOS flat group this removes every
  line of the group), and a changed one is entered by its header with the changed lines
  inside. A changed value on the header line (a route-map's action) re-enters the block with
  the new header;
- positional (`ordered`) collections are replaced whole when anything differs: every running
  element is removed, then every intent element is added in order;
- singletons compare presence (a different key is a remove plus an add).

For each field:

| running → intent | command |
|---|---|
| same | nothing |
| anything → a different value | the value line |
| value or missing → `null` | the negated form (`no remote-as`) |
| anything → key missing | nothing: missing means "no opinion" (unless `--explicit`) |
| flag changes | the positive or negated line; back to a defaulted value: its negated form |

With `--explicit`, intent is the complete desired state: a missing value is cleared, a missing
flag or defaulted field is its default, and a missing collection is empty. Removals come
before additions at each level. Unmanaged lines of the running config are never touched;
`--show-unmanaged` lists them on stderr. `--format json` gives a flat list of operations,
`{"op": "set"|"delete", "path": [enclosing headers], "line": ..., "was": the running line it
replaces}`, with the provenance fields. Library: `Engine::diff(model, &running, &intent)` →
`ChangeSet` (`to_text`, `to_json`, `ops`); Python: `Engine.diff(model, running, intent)` →
`ChangeSet` with `.text`, `.ops`, `.empty`.

Braces dialects need `render: set` for change sets (`set ...` / `delete ...` lines); structured
braces output has no way to delete, so `diff` refuses it.

**Limits.** A change set is correct for a device that (1) replaces a setting when the same
command is given with a new value, (2) clears a setting with `<negation> <the line's
literals>` and (3) removes a block with `<delete> <header>`. That holds for the common IOS,
NX-OS and EOS commands modelled here, and the test suite checks it by applying change sets
to rendered configs and re-parsing. It does not hold for additive commands modelled as a
single value (`ntp server X` modelled as one value would add a second server rather than
replace the first; model such lines as a keyed collection), and a device may print a
negated line differently from how it accepts it. Headers whose value changes are re-entered
with the new value, which relies on the device editing the entry in place.

## Render modes and provenance

`render` writes canonical config by default (`--canonical`): flags and defaulted fields only
when they differ from their declared defaults, so the output parses back to the same data.
`render --explicit` (`RenderMode::Explicit`, Python `render(..., explicit=True)`) writes every
flag and defaulted field, including defaults that have a spelling.

Parse results carry `engine_version` and `templates_version` (the set manifest's `version`,
`"unversioned"` for directory loads). `parse --format json` prints an envelope,
`{"engine_version", "templates_version", "model", "value", "unmanaged"}`; YAML output is the
data with the same provenance in a leading comment. `render`, `validate-data` and `diff`
accept either the envelope or plain data.

## Authoring tools

### `netcfg test`: template tests

Tests live next to the templates in `*.test.nct` files (never loaded as templates):

```text
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

`expect` is YAML compared exactly in canonical form: flags and defaulted fields at their
defaults and empty collections may be left out (canonical rendering writes nothing for them),
but every other key must match and missing keys must be missing. `unmanaged` is the exact
list of unmanaged paths; `roundtrip: true` renders the parse and parses it again; `render:`
is the exact canonical rendering; `fails:` requires a parse error containing the text. A block
property is written `key:` or `key: |` with the block indented under it. `netcfg test
templates/eos` (or `--set`) prints pass/fail per test and exits non-zero on failure. Each
example set has a test file porting part of the Rust edge-case suite.

### `netcfg check`: golden configs as a CI gate

```
netcfg check --set templates/nxos/set.nct --golden templates/nxos/golden            # write reports
netcfg check --set templates/nxos/set.nct --golden templates/nxos/golden --compare  # CI
```

Every file under the golden directory (except dot-files) is parsed with the set's device model
(the manifest's `root`, or `--model`); a strict error fails the check. Each config's unmanaged
report is written to `golden/.unmanaged/<config>.txt`; with `--compare` the reports are
compared with the committed ones instead, and any difference (`- removed`, `+ added`) fails.
Commit the reports; a template change that changes what is managed then shows up in review.

### `netcfg suggest`: from unmanaged lines to template lines

```
$ netcfg suggest templates/nxos templates/nxos/golden/leaf1.cfg --model Device
router bgp 65000 > neighbor * > bfd           (2x)  bfd [[ bfd ]]                 bfd: flag
router bgp 65000 > neighbor * > password 3 *  (2x)  password 3 {{ password }}     password: string?
feature bgp                                   (1x)  feature bgp [[ featureBgp ]]  featureBgp: flag
version *                                     (1x)  version {{ version }}         version: string?
```

Unmanaged leaf lines are grouped under their ancestors (tokens that vary between sibling
ancestors become `*`) by literal prefix, the tokens up to the first one that varies. A
constant line proposes a flag; one varying trailing token proposes `string?` (`int?`,
`ipv4?`, `ipv6?` when every value is one); several propose `phrase?`. A varying token followed
by repeated keywords is taken as a key (`neighbor {{ key }} bfd` for EOS flat groups).
Groups an `@ignore` already covers are marked. These are heuristics; nothing is edited.

### `netcfg lint`

Warnings (exit status 0 unless `--strict`):

- a template line **shadowed** by an earlier line with the same literal prefix that claims or
  rejects every such line (`hops {{ a }}` before `hops {{ b }}`);
- **ambiguous claims**: nested models (or a nested model and a line) at the same level whose
  entry lines share a literal prefix;
- with `--golden dir`: models that never appear, value fields never set, flags that are always
  at their declared default (is it the device's default? is the flag needed?), and `@ignore`
  prefixes that match no line.

Library: `Engine::run_tests_in`, `check_goldens`, `suggest`, `lint`; Python:
`Engine.run_tests()`, `check_goldens(dir, compare=...)`, `suggest(model, text)`,
`lint(golden=...)`.

## Data validation

`render` and `validate-data` apply the same checks to data: unknown fields, wrong types,
missing required fields, struct shapes, duplicate keys within a collection, and cross-field
references. Every message
carries the data path (`Device.bgp[0].neighbors[1]: field `remoteAs`: "x" is not an AS
number`). `render` stops at the first problem; `validate-data` (and
`Engine::validate_data`) reports all of them. Since 0.4 `render` rejects unknown fields
instead of ignoring them.

## Matching rules

Every line under a block the model owns ends up in exactly one place: **claimed** by the first template line that fully matches (template order); an **error** if it starts like a managed line (literals and key placeholders up to the first value placeholder match) but nothing fully matches, since silently leaving the field at its default would misrepresent the device; otherwise **unmanaged**, reported with its ancestors (`router bgp 65000 > neighbor 10.1.0.1 > bfd`). `@ignore` prefixes are checked first and always win. Headers are never strict: a `route-map` line whose sequence number doesn't decode is simply not one of ours.

**Constant lines** are claimed like any other line and produce no data. If one is missing
from its block (or, in a flat group, from any key's lines), parsing fails:
``in `address-family ipv4 unicast`: constant line `exit-address-family` is missing (if this
line is optional, declare a flag and write `exit-address-family [[ name ]]`)``. Present twice is
`matched twice`; with extra tokens, or negated when the template line isn't, it is
unrepresentable. A constant written with the negation word (`no ip domain-lookup`) is
matched literally, so `ip domain-lookup` is then an error. Constants are rendered whatever the
data (including their containers), never appear in the data or the JSON Schema, and in change
sets are only written as part of an added block. An `@ignore` that covers a constant would
make it unmatchable; that is reported when the set loads and by `netcfg lint`. The IOS example
uses `neighbor {{ peer }} activate` in `templates/ios/bgp.nct`. Note that IOS's
`exit-address-family` is printed beside each `address-family` block, not inside it, so it is
`@ignore`d there rather than modelled as a constant.

## CLI

Every command takes the set either as `--set path/to/set.nct` or as a leading TEMPLATES
argument (a manifest, or a directory):

```
cargo build --release
netcfg validate templates                           # every set under templates/
netcfg validate --set templates/nxos/set.nct
netcfg parse --set templates/nxos/set.nct running.cfg --model Device
netcfg validate templates/nxos
netcfg parse   templates/nxos running.cfg --model Device --format yaml --unmanaged
netcfg render  templates/nxos intent.yaml  --model Device   # --explicit writes defaults too
netcfg diff    templates/nxos running.cfg intent.yaml --model Device
netcfg explain templates/ios  --model Interface     # how each field is spelled: value, absent, true/false, defaults
netcfg schema  templates/nxos --model Device        # JSON Schema for editor completion/validation
netcfg skeleton templates/nxos --model Device       # example YAML: every field, typed placeholders, docs
netcfg validate-data templates/nxos intent.yaml --model Device   # check data without rendering; every error
netcfg bench   templates/nxos running.cfg --model Device --runs 5
netcfg fmt     templates/                           # named templates, each after its model (--check, --keep-order)
netcfg test    templates/eos                        # run *.test.nct template tests
netcfg check   --set templates/nxos/set.nct --golden templates/nxos/golden --compare
netcfg suggest templates/nxos running.cfg --model Device
netcfg lint    templates/nxos --golden templates/nxos/golden
```
`--dialect NAME` selects a builtin when the templates don't declare one.
```
```

## Python

```
pip install maturin && cd netcfg-py && maturin build --release   # wheel in target/wheels
```
Bindings use pyo3 0.22, which supports CPython 3.7–3.13. For Python 3.14 bump `pyo3` in
`netcfg-py/Cargo.toml` to 0.25 or later (the Bound API used here is unchanged).
```python
import netcfg
e = netcfg.Engine("templates/nxos")            # a manifest or a directory; dialect from the templates
sets = netcfg.load_all("templates")             # {"eos": Engine, "ios": ..., ...}
e.set_name, e.templates_version                 # "nxos", "2026.09.1"
r = e.parse("Device", text)      # r.value: dict, r.unmanaged: list of paths; ValueError on bad config
r.engine_version, r.templates_version
text = e.render("Device", r.value)
schema = e.schema("Device")
print(e.skeleton("Device"))      # example YAML
errors = e.validate_data("Device", intent)   # [] when valid; same messages render raises
```
The GIL is released during `parse` and `render`, so a thread pool parallelises across cores.
`netcfg-py/smoke.py` exercises the installed wheel (`python netcfg-py/smoke.py`).

## Tests and equivalence

- `cargo test`: unit tests plus the edge-case suite (must-fail, must-be-unmanaged, invariants, must-parse-to) ported from the Scala harness, over NX-OS blocks, EOS flat groups, `@ignore`, and template validation; and one suite per feature area: `nct_format.rs` (file format, named templates, `<< >>`, singletons), `declarations.rs` (docs, comments, mapped enums, fragments, skeleton, validate-data), `collections.rs`, `sets.rs`, `diff.rs` (change sets are applied to rendered configs and re-parsed), `authoring.rs` (test/check/suggest/lint/references).
- `netcfg test templates/<set>` runs each example set's `*.test.nct` file; `netcfg check --compare` keeps the NX-OS golden report in sync.
- `python netcfg-py/smoke.py` against the installed wheel.
- Cross-check against the Scala engine (measured on 0.3, before the 0.4 format changes): on generated 20k-neighbor NX-OS (297k lines) and EOS (228k lines) configs with injected noise, the Rust engine's canonical render of its parse is **byte-identical** to the Scala engine's, and the unmanaged reports are identical (24,849 and 34,759 paths).

## Performance (0.3; 1 vCPU sandbox, `--release`, median of 5)

| config | lines | lex | match | parse total | render |
|---|---:|---:|---:|---:|---:|
| NX-OS, 20k neighbors | 296,740 | 40 ms | 237 ms | 277 ms (1.07 M lines/s) | 215 ms |
| EOS, 20k neighbors | 227,959 | 36 ms | 301 ms | 336 ms (678 k lines/s) | 303 ms |

Comparable to the tuned JVM engine, with no warm-up, no GC tuning and a flat memory profile. From Python add roughly the same again for converting 670k fields into dicts; for a typical device (a few thousand lines) the whole call is ~10 ms.

## Layout

```
netcfg/src/lexer.rs      indent and braces grammars; tokens borrow from the input
netcfg/src/dialect.rs    declared dialects (builtins are declarations too); grammar selection, rendering
netcfg/src/types.rs      Scalar trait and the builtin catalog
netcfg/src/model.rs      .nct file parser (dialect / type / model / fragment / template sections)
netcfg/src/fmt.rs        `netcfg fmt`: rewrite files to named templates
netcfg/src/template.rs   template text parser, shape inference, validation (pure functions)
netcfg/src/engine.rs     compile to patterns/slots; strict node-major parse; render
netcfg/src/schema.rs     JSON Schema from model declarations
netcfg/src/skeleton.rs   example YAML for a model
netcfg/src/set.rs        set manifests: files, includes, version, load_set / load_all
netcfg/src/diff.rs       change sets: running data -> intent data as config commands
netcfg/src/testfile.rs   `netcfg test`: *.test.nct parser and runner
netcfg/src/golden.rs     `netcfg check`: golden configs and unmanaged reports
netcfg/src/suggest.rs    `netcfg suggest`: template lines for unmanaged lines
netcfg/src/lint.rs       `netcfg lint`
netcfg/src/main.rs       CLI
netcfg-py/               PyO3 bindings (maturin)
templates/{ios,nxos,eos,junos} example sets (each with a set.nct manifest and *.test.nct tests;
                         templates/nxos/golden/ holds a golden config and its report)
templates/common/        files shared between sets through `include`
docs/                    design notes
```

## Next

- Language server and TextMate grammar ([scope](docs/language-server.md)).
- A `set`-style input grammar (Junos `display set`, and vendors whose native form is flat paths).
- Folded multi-line fields, `send-community` + `send-community extended` as one value ([design, for review](docs/folded-fields.md)).
- Versioned template variants ([design](docs/template-variants.md)).
- Golden real-device configs per OS version as the test oracle.
