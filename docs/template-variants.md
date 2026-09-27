# Design note: template variants by OS version

Status: design only (plan item 4.2). Not implemented.

## Problem

The same model is sometimes spelled differently across releases of one platform. NX-OS
before 10.x writes `neighbor X remote-as Y` inside `router bgp`; a later release might move a
knob under a sub-mode or rename a keyword. Today the only answer is a second set with a
copy of every model, which drifts. We want one model (one data shape, one JSON Schema) with
more than one template, and a way for a set to pick the right one for a device.

## Syntax

A model keeps exactly one *default* template, `template NAME`, and may add variants that
carry a selector:

```text
model Neighbor
  peer: key ip
  remoteAs: asn?
  bfd: flag

template Neighbor
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
    bfd [[ bfd ]]

template Neighbor @nxos>=10.2
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
    bfd multihop [[ bfd ]]
```

- The selector is `@<platform><op><version>` with `op` one of `>=`, `<`, `=`, and `version`
  a dotted numeric release (`10`, `10.2`, `9.3.8`). Several selectors may be joined with
  `,` meaning *and*: `@nxos>=9.3,nxos<10.2`. Platform names are free-form but conventionally
  the dialect name.
- Versions compare numerically component by component; missing components are zero
  (`10` = `10.0.0`). Vendor suffixes (`9.3(8)`, `22.4R1`) are normalized by a per-platform
  rule declared in the dialect (`version-format: nxos`), never by regex in the template.
- The default template (no selector) applies when no variant matches. A model may not have two
  templates with the same selector (today's "two templates for one model" error becomes
  "two templates for one model with the same selector").
- Every variant is validated against the model like a normal template, and must bind the
  same fields (a variant cannot drop a field without a default), so data for the model is
  the same whichever variant renders it. Fragments may carry variants the same way.

## Selecting a variant

A set declares the target it renders for:

```text
set nxos
  dialect: nxos
  version: 2026.09.1
  target: nxos 10.3
```

- `target` is `<platform> <version>`; it can be overridden per call (`--target "nxos 9.3.8"`,
  `Engine.parse(model, text, target=...)`), since one set usually serves a fleet on mixed
  releases. The device's release can often be read from the config itself (`version 9.3(8)`
  on NX-OS), so a later step could let the root model bind it and select automatically.
- Selection happens per model at compile time: the engine compiles one pattern/slot tree per
  distinct target on first use and caches it, so parse and render cost is unchanged.
- Among the variants whose selector matches, the most specific wins: the one whose version
  range is narrowest; ties are a load error ("ambiguous variants for target nxos 10.3").
- With no `target` and no override, only default templates are used, exactly as today.

## Interaction with the rest of the plan

- **Invertibility and strictness** are per variant: each variant is an ordinary template, so
  nothing about matching changes.
- **`explain`** takes the target and shows the selected variant, naming it
  (`Neighbor (nxos dialect, variant @nxos>=10.2)`).
- **`diff`** compares data, so it is unaffected; the rendered commands use the target's
  variant.
- **`netcfg test`** gains an optional `target:` per test so each variant is exercised.
- **`lint`** warns about variants whose ranges overlap without one being narrower, and about
  variants that are textually identical to the default.

## Out of scope

Selecting on anything other than platform and version (hardware model, feature licences),
and variants that change the data shape. Both would break the "one model, one schema" rule
that makes variants cheap.
