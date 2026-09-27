# netcfg (Rust)

Bidirectional network-config templates as a Rust core, a CLI, and a Python package.
Authors write `.ttp` files (a model declaration plus config-shaped template text); the
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
  matchPrefixLists: names?
  setLocalPref: int?

template
  route-map {{ name }} {{ action }} {{ seq }}
    description {{ description }}
    match ip address prefix-list {{ matchPrefixLists }}
    set local-preference {{ setLocalPref }}
```

## The `.ttp` format

| Declaration | Meaning |
|---|---|
| `f: key T` | `f` is part of the block's identity; several keys form a composite key. Keys go on the header line (blocks) or on every line (flat groups). |
| `f: T` | Required value. May also appear on the header line (`route-map {{ name }} {{ action }} {{ seq }}`). |
| `f: T = default` | Required, with a default used when the line is absent; the line is omitted when rendering the default. |
| `f: T?` | The line is optional. |
| `f: flag` / `f: flag = true` (written `[[ f ]]` in the template) | Presence of a literal line; `<negation> <line>` is `false`. Set the default to the device's default so negated lines render exactly when needed. A flag whose template literals start with the negation word (`no ip address {{ cleared }}`) is matched literally and only has that spelling. |
| `f: [Model]` | A keyed collection; `{{ f }}` alone on a line stands for all of its blocks/lines. |
| `type name = /regex/` | One-token type validated by a regex. |
| `type name = "a" \| "b"` | Enumeration of literal tokens (quoted). |
| `type name = asn \| "auto"` | Union: alternatives tried in order; bare names are types, quoted words are literals. |
| `type name = string \| ""` | An empty literal matches nothing at the end of the line: a value that may be present without a value (`neighbor X group` vs `neighbor X group CORE`, data `""` vs `"CORE"`). Must be the last placeholder. |
| `type name = {{ limit: int }} {{ action: "warning-only" \| "" }}` | Struct type: a value's own little template. Data is a record (`{limit: 1200, action: warning-only}`); sub-fields whose type allows `""` may sit anywhere and are omitted when absent. Placeholders may use inline unions. |
| `f: {{ limit: int }} {{ action: "warning-only" \| "" }}?` | Anonymous struct on a field, for one-off shapes; `type` is for reused ones. |
| `list(T)` | Rest-of-line list of `T` (`prependAsPath: list(prependItem)?`). `T` must be a one-token type. |
| `@ignore word word *` | Explicit opt-out: lines starting with these words are reported as unmanaged, never errors. |

Builtin types: `string`, `int`, `int(lo..hi)`, `list(T)`, `ipv4`, `ipv6`, `ip` (either), `cidr` (dialect-dependent: `addr/len` on NX-OS/EOS, `addr mask` on IOS; the value is always `addr/len`), `ipv6cidr`, `prefix` (either), `asn` (asplain or asdot in, asplain out), `intpair`, and the rest-of-line types `phrase`, `names`, `ints`. Domain types with real parsing logic are added in Rust by implementing the `Scalar` trait.

Template shapes are inferred: one header line with nested lines is a **block**; several sibling lines that all carry the key are a **flat group** (EOS/IOS `neighbor X …` lines); a model without keys is a **root** document.

## Dialects

A dialect is declared, not coded, in a `dialect` section (conventionally `dialect.ttp` next to the templates):

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
template
  system {
      host-name {{ hostname }};
  }
  protocols {
      bgp {
          {{ groups }}
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
netcfg bench   templates/nxos running.cfg --model Device --runs 5
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
```
The GIL is released during `parse` and `render`, so a thread pool parallelises across cores.

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
netcfg/src/model.rs      .ttp file parser (type / model / template sections)
netcfg/src/template.rs   template text parser, shape inference, validation (pure functions)
netcfg/src/engine.rs     compile to patterns/slots; strict node-major parse; render
netcfg/src/schema.rs     JSON Schema from model declarations
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
