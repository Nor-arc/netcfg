# Design note: folded fields

Status: design for review (plan Phase 7). Not implemented.

## Problem

Some settings are spelled as several lines in one dialect and one line in another, or as a
combination of lines whose meaning is the combination. The standard example is BGP
community sending:

| platform | config | meaning |
|---|---|---|
| NX-OS | `send-community` | standard |
| NX-OS | `send-community extended` | extended |
| NX-OS | `send-community` + `send-community extended` | both |
| EOS / IOS-XE | `send-community both` | both |

Today NX-OS models this as two flags (`sendCommunity`, `sendCommunityExtended`) and EOS as
one value. The data differs per platform for the same intent, which defeats one schema for
a fleet. A *folded field* is one value in the data that is spelled as a set of lines.

## Proposal

Declare the field with an enum type as usual, and mark each line of the fold with the part
of the value it contributes, using the existing flag marker with a part name:

```text
type communities = "standard" | "extended" | "both"
  fold both = standard + extended

model NeighborAf
  afi: key string
  safi: key string
  sendCommunity: communities?

template NeighborAf
  address-family {{ afi }} {{ safi }}
    send-community [[ sendCommunity: standard ]]
    send-community extended [[ sendCommunity: extended ]]
```

- `[[ field: part ]]` is a flag-like line: its presence contributes `part`. A field may be
  bound by several such lines; each part appears once.
- The type's `fold` lines map a *set* of parts to a value that no single line spells
  (`both = standard + extended`). A part name that is also an alternative maps to itself.
- Parse collects the parts present at this level and maps the set to a value. A set with no
  mapping is a strict error (`send-community extended` + a hypothetical third part that no
  fold names), never silently dropped.
- Render writes the lines of every part in the value's set, in template order.
- `null` is the negated form of every line in the fold (`no send-community`,
  `no send-community extended`); key missing writes nothing, as for any value.

EOS keeps its single-line form (`send-community {{ sendCommunity }}`) with the same type, so
both platforms produce `sendCommunity: both`.

## Load-time checks (invertibility)

- Every fold set maps to exactly one value and every value to exactly one set; two values
  with the same set, or a set that is a value's own singleton under another name, is an error.
- The empty set is not a value (that is "missing"); a type that needs "none" spells it with a
  negated line instead.
- All lines of a fold sit at the same level of the same model, and none may also bind a
  `{{ }}` value (keeps the line's presence the only information).
- A fold field cannot be a key and cannot have a default (the default would need a spelling
  per part; revisit if a real case needs it).

## Change sets

`diff` compares the folded value. Moving from `both` to `standard` removes the lines of the
parts that leave (`no send-community extended`) and adds those that arrive; `null` negates
every line of the fold. Because the lines are written per part, a change never touches the
lines of parts that stay.

## Alternatives considered

1. **A fourth marker** (`(( sendCommunity ))`) for fold lines. Rejected for now: the plan fixed
   three markers, and a part is presence information, which is what `[[ ]]` already means.
2. **Folding in the model, not the type** (`sendCommunity: fold { standard: ..., extended:
   ... }`). Puts config spellings into the model section, which is supposed to be
   dialect-free; the type-level `fold` keeps spellings in templates and data values in types.
3. **Leave it to the data layer** (two flags, and a mapping in the caller). Works today, but
   every consumer re-implements it and the schema differs per platform.

## Open questions for review

- Do we need folds across levels (a part inside a sub-mode)? None of the known cases do.
- Should `explain` show the fold table (value → lines) or per-line parts? Proposal: the table.
- Migration: the NX-OS example's two flags would become one field; that changes the data, so
  it would be a versioned template-set change (`version:` bump), not a silent one.
