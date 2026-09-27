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

A set is a directory of `.nct` files. Each holds `dialect`, `type`, `model` (and
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

Properties: `grammar`, `indent`, `comments`, `skip`, `block-separator`, `end-marker`, `negation` (the prefix that negates a flag line, `no`), `render`, and type knobs such as `cidr`. Templates are lexed with the dialect's grammar, so a Junos template is written in Junos syntax:

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

## Data validation

`render` and `validate-data` apply the same checks to data: unknown fields, wrong types,
missing required fields, struct shapes, and duplicate keys within a collection. Every message
carries the data path (`Device.bgp[0].neighbors[1]: field `remoteAs`: "x" is not an AS
number`). `render` stops at the first problem; `validate-data` (and
`Engine::validate_data`) reports all of them. Since 0.4 `render` rejects unknown fields
instead of ignoring them.

## Matching rules

Every line under a block the model owns ends up in exactly one place: **claimed** by the first template line that fully matches (template order); an **error** if it starts like a managed line (literals and key placeholders up to the first value placeholder match) but nothing fully matches, since silently leaving the field at its default would misrepresent the device; otherwise **unmanaged**, reported with its ancestors (`router bgp 65000 > neighbor 10.1.0.1 > bfd`). `@ignore` prefixes are checked first and always win. Headers are never strict: a `route-map` line whose sequence number doesn't decode is simply not one of ours.

## CLI

```
cargo build --release
netcfg validate templates/nxos
netcfg parse   templates/nxos running.cfg --model Device --format yaml --unmanaged
netcfg render  templates/nxos intent.yaml  --model Device
netcfg explain templates/ios  --model Interface     # how each field is spelled: value, absent, true/false, defaults
netcfg schema  templates/nxos --model Device        # JSON Schema for editor completion/validation
netcfg skeleton templates/nxos --model Device       # example YAML: every field, typed placeholders, docs
netcfg validate-data templates/nxos intent.yaml --model Device   # check data without rendering; every error
netcfg bench   templates/nxos running.cfg --model Device --runs 5
netcfg fmt     templates/                           # named templates, each after its model (--check, --keep-order)
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
e = netcfg.Engine("templates/nxos")            # dialect comes from the templates' declaration
r = e.parse("Device", text)      # r.value: dict, r.unmanaged: list of paths; ValueError on bad config
text = e.render("Device", r.value)
schema = e.schema("Device")
print(e.skeleton("Device"))      # example YAML
errors = e.validate_data("Device", intent)   # [] when valid; same messages render raises
```
The GIL is released during `parse` and `render`, so a thread pool parallelises across cores.
`netcfg-py/smoke.py` exercises the installed wheel (`python netcfg-py/smoke.py`).

## Tests and equivalence

- `cargo test`: unit tests plus the edge-case suite (must-fail, must-be-unmanaged, invariants, must-parse-to) ported from the Scala harness, over NX-OS blocks, EOS flat groups, `@ignore`, and template validation.
- Cross-check against the Scala engine: on generated 20k-neighbor NX-OS (297k lines) and EOS (228k lines) configs with injected noise, the Rust engine's canonical render of its parse is **byte-identical** to the Scala engine's, and the unmanaged reports are identical (24,849 and 34,759 paths).

## Performance (1 vCPU sandbox, `--release`, median of 5)

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
netcfg/src/main.rs       CLI
netcfg-py/               PyO3 bindings (maturin)
templates/{ios,nxos,eos,junos} example dialects and models
```

## Next

- Language server over `template.rs` / `model.rs` (diagnostics, completion of fields and types) and a TextMate grammar.
- A `set`-style input grammar (Junos `display set`, and vendors whose native form is flat paths).
- Folded multi-line fields, ordered `seq` collections (ACLs, route-map `continue`).
- Golden real-device configs per OS version as the test oracle.
- Typed diff of two model values → minimal config change.
