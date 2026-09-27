# Scope: `netcfg-lsp` and a TextMate grammar

Status: scoping (plan Phase 7), written after Phases 1–6. Not started.

## Goal

Editing `.nct` files in any LSP editor with the same rules the engine enforces: errors as
you type, completion of field and type names, and `explain` on hover. The server is a thin
layer over `model.rs` / `template.rs` / `Engine::build`, so there is exactly one definition of
what a valid set is.

## Prerequisite in the core: structured diagnostics

Load errors are strings today (`file:line: message`, `template line N ...`). The server needs
`Diagnostic { file, line, column_range, severity, message, fix: Option<String> }`.

- Add the type in `netcfg` and make `model::parse`, `template::validate` and `Engine::build`
  return `Vec<Diagnostic>` internally; `Error` stays the public string form by formatting
  them, so the CLI and Python bindings do not change.
- Token columns come from the lexer (it already knows byte offsets; `Node` needs `col`).
- `lint` warnings become diagnostics with `severity: warning`.

This is the largest piece of work and is useful without the server (CLI output with
columns, JSON diagnostics for CI).

## Server features, in order

1. **Diagnostics** on open/change for the whole set the file belongs to (its manifest, or its
   directory): parse errors, template validation, pairing errors (model without template),
   fragment errors, and lint warnings. Re-build the set on each change; sets are small, and
   build is milliseconds.
2. **Completion**
   - inside `{{ }}`: value fields of the model the template names; inside `[[ ]]`: flags;
     inside `<< >>`: `[Model]`/`Model?` fields and `@Fragment` names;
   - after `field:` in a model: builtin types, declared types, models (for nesting), `flag`,
     `list(`, `int(`;
   - `template ` → model and fragment names without a template yet;
   - in a `set` section: property names; in `dialect`: property names and builtin bases.
3. **Hover**: on a placeholder, the field's `explain` rows (value / missing / null / flag
   spellings) and its doc; on a type name, its definition and alternatives.
4. **Go to definition / references**: placeholder → field declaration; field type → type or
   model; `template X` ↔ `model X`; `<< @F >>` → fragment; `references M.k` → `M`.
5. **Formatting**: `fmt.rs` as the document formatter.
6. **Code actions**: from diagnostics that carry a fix (`{{ flag }}` → `[[ flag ]]`, removed
   builtin types → replacement, bare `template` → `template NAME`), and "add template line"
   from `suggest` when a golden directory is configured.
7. **Test files** (`*.test.nct`): diagnostics from the test parser, and a code lens "run" per
   test that shows the outcome inline.

## TextMate grammar

A separate `editors/nct.tmLanguage.json` (usable by VS Code, Sublime, GitHub linguist):
section keywords (`set`, `dialect`, `type`, `model`, `fragment`, `template`, `test`), `#`
comments outside templates and `##` inside, field declarations (`name: key type? = default #
doc`), the three markers with distinct scopes, quoted literals and `->` mappings in types,
`@ignore` lines. Template text is otherwise plain (it is config).

## Crate layout

```
netcfg-lsp/            tower-lsp server; depends on netcfg with default-features = false
  src/main.rs          stdio transport
  src/workspace.rs     file -> set resolution (manifest discovery), rebuild on change
  src/complete.rs, hover.rs, goto.rs, actions.rs
editors/vscode/        extension: grammar + client that launches netcfg-lsp
```

## Estimate and milestones

| milestone | contents | size |
|---|---|---|
| M1 | structured diagnostics in the core, CLI prints columns | medium |
| M2 | server with diagnostics + formatting, VS Code extension, TextMate grammar | medium |
| M3 | completion and hover | medium |
| M4 | goto/references, code actions, test code lens | small–medium |

## Out of scope

Editing config files themselves (a config is not a `.nct` file); a web playground; semantic
tokens beyond the TextMate grammar.
