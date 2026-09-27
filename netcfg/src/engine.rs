//! Compiles `.nct` models into patterns and slots, then parses and renders with them.
//!
//! Matching is node-major and strict: each config line is offered to the slots in template
//! order; the first full match claims it. An unclaimed line that *starts like* a managed
//! line is an error unless an `@ignore` prefix covers it; other unclaimed lines are
//! reported as unmanaged, with their ancestors, so the report reads like config.

use crate::dialect::Dialect;
use crate::lexer::{Node, OwnedNode};
use crate::model::{self, FieldDef, Kind, TypeBody};
use crate::template::{self, Shape, TLine, Tok};
use crate::types::{Alt, Catalog, RegexType, SToken, ScalarRef, StructType, UnionType};
use crate::value::{Record, Value};
use crate::{Error, Result};
use indexmap::IndexMap;
use std::collections::HashSet;
use std::sync::Arc;

// ---- compiled form ------------------------------------------------------------------------

enum PTok {
    Lit(String),
    Hole { field: usize, ty: ScalarRef, key: bool },
}

pub struct Pattern {
    toks: Vec<PTok>,
}

enum PRes {
    Ok(Vec<(usize, Value)>, usize),
    NoMatch,
    Bad(String),
}

impl Pattern {
    fn show(&self, fields: &[FieldDef]) -> String {
        self.toks.iter().map(|t| match t {
            PTok::Lit(s) => s.clone(),
            PTok::Hole { field, .. } => format!("{{{{ {} }}}}", fields[*field].name),
        }).collect::<Vec<_>>().join(" ")
    }

    /// Match as a prefix; returns the values and how many tokens were consumed.
    fn parse_prefix(&self, toks: &[&str]) -> PRes {
        let mut vals = Vec::new();
        let mut pos = 0;
        for t in &self.toks {
            match t {
                PTok::Lit(s) => {
                    if pos < toks.len() && toks[pos] == s { pos += 1; } else { return PRes::NoMatch; }
                }
                PTok::Hole { field, ty, .. } => match ty.parse(&toks[pos..]) {
                    Ok((v, n)) => { vals.push((*field, v)); pos += n; }
                    Err(e) => return PRes::Bad(e),
                },
            }
        }
        PRes::Ok(vals, pos)
    }
    fn parse_full(&self, toks: &[&str]) -> PRes {
        match self.parse_prefix(toks) {
            PRes::Ok(v, n) if n == toks.len() => PRes::Ok(v, n),
            PRes::Ok(..) => PRes::NoMatch,
            other => other,
        }
    }
    /// The literal tokens before the first hole (`ip address` for `ip address {{ address }}`).
    fn literal_prefix(&self) -> Vec<&str> {
        self.toks.iter().take_while(|t| matches!(t, PTok::Lit(_))).map(|t| match t { PTok::Lit(s) => s.as_str(), _ => unreachable!() }).collect()
    }
    fn has_holes(&self) -> bool { self.toks.iter().any(|t| matches!(t, PTok::Hole { .. })) }
    fn hole_fields(&self) -> Vec<usize> { self.toks.iter().filter_map(|t| match t { PTok::Hole { field, .. } => Some(*field), _ => None }).collect() }

    /// The line starts like this pattern: literals and key holes match up to the first value hole.
    fn collides(&self, toks: &[&str]) -> bool {
        let mut pos = 0;
        for t in &self.toks {
            match t {
                PTok::Lit(s) => { if pos < toks.len() && toks[pos] == s { pos += 1; } else { return false; } }
                PTok::Hole { key: true, ty, .. } => match ty.parse(&toks[pos..]) { Ok((_, n)) => pos += n, Err(_) => return false },
                PTok::Hole { key: false, .. } => return true,
            }
        }
        true
    }
    fn render(&self, fields: &[FieldDef], rec: &Record) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for t in &self.toks {
            match t {
                PTok::Lit(s) => out.push(s.clone()),
                PTok::Hole { field, ty, .. } => {
                    let f = &fields[*field];
                    let v = rec.get(&f.name).ok_or_else(|| Error(format!("field `{}` is missing", f.name)))?;
                    out.extend(ty.encode(v).map_err(|e| Error(format!("field `{}`: {e}", f.name)))?);
                }
            }
        }
        Ok(out)
    }
}

enum Mode {
    Required,
    Opt,
    Default(Value),
}

enum Slot {
    Line { pat: Pattern, mode: Mode },
    Flag { lits: Pattern, field: usize, default: bool },
    /// `<< field >>`: blocks/groups of a nested model.
    Nested { field: usize, model: usize, card: Card },
    /// A literal-only line with nested lines (`protocols {`, `bgp {`): its children are
    /// body lines of the same model.
    Container { lits: Pattern, body: Vec<Slot>, ignores: Vec<Vec<String>> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Card {
    Many,
    Single { required: bool },
}

enum CShape {
    Block { header: Pattern, key_fields: Vec<usize>, body: Vec<Slot> },
    Flat { keys: Pattern, lines: Vec<Slot> },
    Root { body: Vec<Slot> },
}

pub struct Compiled {
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub doc: Option<String>,
    shape: CShape,
    /// `@ignore` prefixes: for blocks/roots they apply to the body; for flat groups to the parent level.
    ignores: Vec<Vec<String>>,
    /// Field types, for schema generation (`None` for flags and collections).
    field_types: Vec<Option<ScalarRef>>,
}

impl Compiled {
    pub fn is_keyed(&self) -> bool { !matches!(self.shape, CShape::Root { .. }) }
    pub fn field_type(&self, i: usize) -> Option<&ScalarRef> { self.field_types[i].as_ref() }
}

/// A loaded set of models for one dialect.
pub struct Engine {
    pub dialect: Dialect,
    pub catalog: Catalog,
    models: IndexMap<String, Compiled>,
    /// Non-fatal notes from loading (deprecations), each naming file and line.
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Engine({}, models: {:?})", self.dialect.name, self.model_names())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub value: Value,
    pub unmanaged: Vec<OwnedNode>,
}

impl Parsed {
    pub fn unmanaged_paths(&self) -> Vec<String> { OwnedNode::leaf_paths(&self.unmanaged) }
}

// ---- compilation --------------------------------------------------------------------------

impl Engine {
    /// Compile a set of parsed `.nct` files. The dialect comes from a `dialect` section in
    /// the files if there is one, otherwise from `fallback` (a builtin name).
    pub fn build(files: &[model::File], fallback: Option<&str>) -> Result<Engine> {
        let declared: Vec<&model::DialectDef> = files.iter().filter_map(|f| f.dialect.as_ref()).collect();
        let dialect = match declared.as_slice() {
            [] => match fallback {
                Some(n) => Dialect::builtin(n).ok_or_else(|| Error(format!("unknown dialect `{n}` (builtin: {}); declare one with a `dialect` section", Dialect::builtin_names().join(", "))))?,
                None => return Err(Error(format!("no `dialect` section found and no dialect given (builtin: {})", Dialect::builtin_names().join(", ")))),
            },
            [d] => Dialect::from_props(&d.name, &d.props).map_err(|e| Error(format!("dialect {}: {}", d.name, e.0)))?,
            many => return Err(Error(format!("more than one dialect declared: {}", many.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")))),
        };
        let mut catalog = Catalog::builtin(&dialect.knobs).map_err(Error)?;
        let mut errors: Vec<String> = Vec::new();
        let warnings: Vec<String> = files.iter().flat_map(|f| f.warnings.iter().cloned()).collect();
        for t in files.iter().flat_map(|f| f.types.iter()) {
            if catalog.get(&t.name).is_some() { errors.push(format!("type `{}` (line {}) is already defined", t.name, t.line)); continue; }
            match &t.def {
                TypeBody::Regex(r) => catalog.add(Arc::new(RegexType { name: t.name.clone(), source: r.clone(), re: regex::Regex::new(&format!("^(?:{r})$")).map_err(|e| Error(e.to_string()))? })),
                TypeBody::Union(alts) => match resolve_alts(&catalog, alts) {
                    Ok(resolved) => catalog.add(Arc::new(UnionType { name: t.name.clone(), alts: resolved })),
                    Err(e) => errors.push(format!("type `{}` (line {}): {e}", t.name, t.line)),
                },
                TypeBody::Struct(toks) => match struct_type(&catalog, &t.name, toks) {
                    Ok(st) => catalog.add(st),
                    Err(e) => errors.push(format!("type `{}` (line {}): {}", t.name, t.line, e.0)),
                },
            }
        }
        let mut defs: Vec<model::ModelDef> = files.iter().flat_map(|f| f.models.iter().cloned()).collect();
        let mut seen: IndexMap<String, usize> = IndexMap::new();
        for (i, d) in defs.iter().enumerate() {
            if let Some(&j) = seen.get(&d.name) { errors.push(format!("{}:{}: model `{}` is already defined at {}:{}", d.source, d.line, d.name, defs[j].source, defs[j].line)); }
            else { seen.insert(d.name.clone(), i); }
        }
        // Pair every `template NAME` with its model or fragment, by name, across the set.
        let mut paired: IndexMap<String, &model::TemplateDef> = IndexMap::new();
        for t in files.iter().flat_map(|f| f.templates.iter()) {
            let Some(&i) = seen.get(&t.model) else {
                let names: Vec<&str> = seen.keys().map(String::as_str).collect();
                errors.push(format!("{}:{}: `template {}` names no model or fragment (known: {})", t.source, t.line, t.model, names.join(", ")));
                continue;
            };
            if let Some(first) = paired.get(&t.model) {
                errors.push(format!("{}:{}: second template for {} (the first is at {}:{}); a model has exactly one template", t.source, t.line, t.model, first.source, first.line));
                continue;
            }
            paired.insert(t.model.clone(), t);
            let d = &mut defs[i];
            d.template = t.text.clone();
            d.template_line = t.first_line;
            d.template_source = t.source.clone();
        }
        for (i, d) in defs.iter().enumerate() {
            if !paired.contains_key(&d.name) && seen.get(&d.name) == Some(&i) {
                errors.push(format!("{}:{}: {} {} has no template; add a `template {}` section", d.source, d.line, if d.fragment { "fragment" } else { "model" }, d.name, d.name));
            }
        }
        // A field whose type names a model is a nested singleton: `ospf: Ospf?` / `ospf: Ospf`.
        let is_model = |n: &str| seen.contains_key(n);
        for d in defs.iter_mut() {
            for f in d.fields.iter_mut() {
                if matches!(f.kind, Kind::Opt | Kind::Scalar) && is_model(&f.type_spec) {
                    if catalog.resolve(&f.type_spec).is_some() {
                        errors.push(format!("{}:{}: field `{}`: `{}` names both a type and a model; rename one", d.source, f.line, f.name, f.type_spec));
                    } else if f.default.is_some() {
                        errors.push(format!("{}:{}: field `{}`: a nested model cannot have a default", d.source, f.line, f.name));
                    } else {
                        f.kind = Kind::Single { required: f.kind == Kind::Scalar };
                    }
                }
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        let rest = |f: &FieldDef| catalog.resolve(&f.type_spec).map(|t| t.rest_of_line()).unwrap_or(false);
        // Fragments: validated on their own (as unkeyed bodies), then spliced where included.
        let mut frags: IndexMap<&str, (&model::ModelDef, Vec<TLine>)> = IndexMap::new();
        for d in defs.iter().filter(|d| d.fragment) {
            let mut errs: Vec<String> = Vec::new();
            for f in &d.fields {
                if f.kind == Kind::Key { errs.push(format!("field `{}`: a fragment has no identity of its own, so it cannot declare keys", f.name)); }
                if f.kind.is_nested() { errs.push(format!("field `{}`: fragments may not contain nested models", f.name)); }
            }
            let mut lines = template::from_nodes(&dialect.lex_template(&d.template), d.template_line).unwrap_or_else(|es| { errs.extend(es); Vec::new() });
            fn mark(ls: &mut [TLine], name: &str, errs: &mut Vec<String>) {
                for l in ls.iter_mut() {
                    l.origin = Some(name.to_string());
                    if let Some(inc) = l.include() { errs.push(format!("{} `<< @{inc} >>`: fragments may not include other fragments", l.at())); }
                    mark(&mut l.children, name, errs);
                }
            }
            mark(&mut lines, &d.name, &mut errs);
            if errs.is_empty() { errs.extend(template::validate(&d.name, &lines, &d.fields, &rest, dialect.negation.as_deref())); }
            if errs.is_empty() { frags.insert(&d.name, (d, lines)); }
            else { errors.push(format!("{}:{} fragment {}: \n  - {}", d.source, d.line, d.name, errs.join("\n  - "))); }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        let model_names: Vec<String> = defs.iter().filter(|d| !d.fragment).map(|d| d.name.clone()).collect();
        let defs: Vec<model::ModelDef> = defs.iter().filter(|d| !d.fragment).cloned().collect();
        // Splice fragments into each model: their lines at the include, their fields merged.
        let mut spliced: Vec<Vec<TLine>> = Vec::new();
        let mut defs = defs;
        for d in defs.iter_mut() {
            let mut errs = Vec::new();
            let lines = template::from_nodes(&dialect.lex_template(&d.template), d.template_line).unwrap_or_else(|es| { errs.extend(es); Vec::new() });
            let mut bound: Vec<String> = Vec::new();
            let lines = expand_fragments(lines, &mut d.fields, &frags, &model_names, &mut bound, &mut errs);
            if !errs.is_empty() { errors.push(format!("{}:{} model {}: \n  - {}", d.source, d.line, d.name, errs.join("\n  - "))); }
            spliced.push(lines);
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        let defs: Vec<&model::ModelDef> = defs.iter().collect();
        let index: IndexMap<&str, usize> = defs.iter().enumerate().map(|(i, d)| (d.name.as_str(), i)).collect();
        let is_keyed = |name: &str| index.get(name).map(|&i| defs[i].fields.iter().any(|f| f.kind == Kind::Key));

        let mut models: IndexMap<String, Compiled> = IndexMap::new();
        for (d, lines) in defs.iter().zip(spliced) {
            let ctx = |m: String| format!("{}:{} model {}: {m}", d.source, d.line, d.name);
            let mut errs: Vec<String> = Vec::new();
            // Resolve types.
            let mut field_types: Vec<Option<ScalarRef>> = Vec::new();
            for f in &d.fields {
                match f.kind {
                    Kind::Flag => field_types.push(None),
                    Kind::Many => {
                        match is_keyed(&f.type_spec) {
                            None => errs.push(format!("field `{}`: unknown model `{}`", f.name, f.type_spec)),
                            Some(false) => errs.push(format!("field `{}`: model `{}` needs at least one key field to be used in a collection", f.name, f.type_spec)),
                            Some(true) => {}
                        }
                        field_types.push(None);
                    }
                    Kind::Single { .. } => {
                        if !index.contains_key(f.type_spec.as_str()) { errs.push(format!("field `{}`: `{}` is a fragment; include it with << @{} >> instead", f.name, f.type_spec, f.type_spec)); }
                        field_types.push(None);
                    }
                    _ if f.type_spec.contains("{{") => match model::parse_struct_body(&f.type_spec).map_err(Error).and_then(|toks| struct_type(&catalog, &format!("{}.{}", d.name, f.name), &toks)) {
                        Ok(t) => field_types.push(Some(t)),
                        Err(e) => { errs.push(format!("field `{}`: {}", f.name, e.0)); field_types.push(None); }
                    },
                    _ => match catalog.resolve(&f.type_spec) {
                        Some(t) => field_types.push(Some(t)),
                        None => { errs.push(format!("field `{}`: {}", f.name, unknown_type(&catalog, &f.type_spec))); field_types.push(None); }
                    },
                }
            }
            if !lines.is_empty() {
                errs.extend(template::validate(&d.name, &lines, &d.fields, &rest, dialect.negation.as_deref()));
            }
            if !errs.is_empty() {
                errors.push(ctx(format!("\n  - {}", errs.join("\n  - "))));
                continue;
            }
            match compile(d, &lines, &field_types, &index, dialect.negation.as_deref()) {
                Ok(c) => { models.insert(d.name.clone(), c); }
                Err(e) => errors.push(ctx(e.0)),
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        // An unkeyed model used as a singleton is identified by its one top-level line.
        for (d, m) in defs.iter().zip(models.values()) {
            for (fi, f) in m.fields.iter().enumerate() {
                if !matches!(f.kind, Kind::Single { .. }) { continue; }
                if let CShape::Root { body } = &models[f.type_spec.as_str()].shape {
                    if body.len() != 1 || matches!(body[0], Slot::Nested { .. }) {
                        errors.push(format!("{}:{} model {}: field `{}`: {} has no key, so as a singleton it is identified by its header line; its template must have exactly one top-level line (not a << >> line), found {}", d.source, d.line, d.name, m.fields[fi].name, f.type_spec, body.len()));
                    }
                }
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        Ok(Engine { dialect, catalog, models, warnings })
    }

    /// Load every `*.nct` file under `dir` (recursively), except `*.test.nct` test files.
    /// `*.ttp` files are still accepted, with a deprecation warning. `fallback` names a
    /// builtin dialect used when the files declare none.
    pub fn load_dir(dir: &std::path::Path, fallback: Option<&str>) -> Result<Engine> {
        let mut paths = Vec::new();
        fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            for e in std::fs::read_dir(p)? {
                let e = e?.path();
                if e.is_dir() { walk(&e, out)?; } else if is_template_file(&e) { out.push(e); }
            }
            Ok(())
        }
        walk(dir, &mut paths).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
        paths.sort();
        Engine::load_files(&paths, fallback)
    }

    /// Load the given template files as one set.
    pub fn load_files(paths: &[std::path::PathBuf], fallback: Option<&str>) -> Result<Engine> {
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for p in paths {
            let text = std::fs::read_to_string(p).map_err(|e| Error(format!("{}: {e}", p.display())))?;
            match model::parse(&p.display().to_string(), &text) {
                Ok(mut f) => {
                    if p.extension().map(|x| x == "ttp").unwrap_or(false) {
                        f.warnings.insert(0, format!("{}: the `.ttp` extension is deprecated; rename the file to `.nct`", p.display()));
                    }
                    files.push(f)
                }
                Err(e) => errors.push(e.0),
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        Engine::build(&files, fallback)
    }

    pub fn from_text(source: &str, text: &str, fallback: Option<&str>) -> Result<Engine> {
        Engine::build(&[model::parse(source, text)?], fallback)
    }

    pub fn model(&self, name: &str) -> Option<&Compiled> { self.models.get(name) }
    pub fn model_names(&self) -> Vec<&str> { self.models.keys().map(String::as_str).collect() }

    fn model_idx(&self, name: &str) -> Result<usize> {
        self.models.get_index_of(name).ok_or_else(|| Error(format!("unknown model `{name}` (known: {})", self.model_names().join(", "))))
    }
}

/// Replace `<< @Fragment >>` lines with the fragment's lines and merge its fields into
/// `fields`, after the last field bound above the include (so data keeps template order).
/// `bound` collects the field names bound so far, in document order.
fn expand_fragments(lines: Vec<TLine>, fields: &mut Vec<FieldDef>, frags: &IndexMap<&str, (&model::ModelDef, Vec<TLine>)>, models: &[String], bound: &mut Vec<String>, errs: &mut Vec<String>) -> Vec<TLine> {
    let mut out = Vec::new();
    for mut l in lines {
        let Some(name) = l.include().map(str::to_string) else {
            bound.extend(l.holes().into_iter().map(String::from));
            let kids = std::mem::take(&mut l.children);
            l.children = expand_fragments(kids, fields, frags, models, bound, errs);
            out.push(l);
            continue;
        };
        let Some((frag, flines)) = frags.get(name.as_str()) else {
            if models.contains(&name) { errs.push(format!("{} `<< @{name} >>`: {name} is a model, not a fragment; nest it with a field (`x: {name}?` or `x: [{name}]`) and << x >>", l.at())); }
            else { errs.push(format!("{} `<< @{name} >>`: no fragment {name} (fragments: {})", l.at(), frags.keys().copied().collect::<Vec<_>>().join(", "))); }
            continue;
        };
        let mut at = bound.iter().filter_map(|b| fields.iter().position(|f| &f.name == b)).max().map(|i| i + 1).unwrap_or(0);
        for f in &frag.fields {
            if fields.iter().any(|g| g.name == f.name) {
                errs.push(format!("{} `<< @{name} >>`: field `{}` of fragment {name} is already a field here", l.at(), f.name));
                continue;
            }
            fields.insert(at, f.clone());
            at += 1;
        }
        for fl in flines {
            fn holes(l: &TLine, bound: &mut Vec<String>) { bound.extend(l.holes().into_iter().map(String::from)); for c in &l.children { holes(c, bound); } }
            holes(fl, bound);
            out.push(fl.clone());
        }
    }
    out
}

/// A set's template files: `*.nct` (and deprecated `*.ttp`), but not `*.test.nct` test files.
pub fn is_template_file(p: &std::path::Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    (name.ends_with(".nct") && !name.ends_with(".test.nct")) || name.ends_with(".ttp")
}

/// Types removed from the builtin catalog, with what to write instead.
const REMOVED_TYPES: &[(&str, &str)] = &[
    ("names", "list(string)"),
    ("ints", "list(int)"),
    ("intpair", "a struct with named parts, e.g. `{{ keepalive: int }} {{ hold: int }}`"),
];

fn unknown_type(catalog: &Catalog, spec: &str) -> String {
    match REMOVED_TYPES.iter().find(|(n, _)| *n == spec) {
        Some((_, instead)) => format!("type `{spec}` was removed; use {instead}"),
        None => format!("unknown type `{spec}` (known: {})", catalog.names().join(", ")),
    }
}

fn struct_type(catalog: &Catalog, name: &str, toks: &[model::StructTok]) -> Result<ScalarRef> {
    let mut out = Vec::new();
    for tok in toks {
        match tok {
            model::StructTok::Lit(l) => out.push(SToken::Lit(l.clone())),
            model::StructTok::Field { name: fname, spec } => {
                let ty: ScalarRef = if spec.contains('|') {
                    let alts = model::parse_alts(spec).map_err(Error)?;
                    Arc::new(UnionType { name: format!("{name}.{fname}"), alts: resolve_alts(catalog, &alts).map_err(Error)? })
                } else {
                    catalog.resolve(spec).ok_or_else(|| Error(format!("field `{fname}`: {}", unknown_type(catalog, spec))))?
                };
                out.push(SToken::Field { name: fname.clone(), ty });
            }
        }
    }
    Ok(Arc::new(StructType { name: name.to_string(), toks: out }))
}

fn resolve_alts(catalog: &Catalog, alts: &[model::Alt]) -> std::result::Result<Vec<Alt>, String> {
    let mut resolved = Vec::new();
    for a in alts {
        match a {
            model::Alt::Lit(l, mapped) => {
                let value = mapped.clone().unwrap_or_else(|| Value::Str(l.clone()));
                if let Some(Alt::Lit(other, _)) = resolved.iter().find(|a| matches!(a, Alt::Lit(_, v) if *v == value)) {
                    return Err(format!("\"{other}\" and \"{l}\" both map to {}; each literal needs its own value so rendering can pick one", fmt_value(&value)));
                }
                resolved.push(Alt::Lit(l.clone(), value));
            }
            model::Alt::Type(n) => match catalog.resolve(n) {
                Some(ty) if ty.rest_of_line() => return Err(format!("`{n}` consumes the rest of the line and cannot be a union alternative")),
                Some(ty) => resolved.push(Alt::Type(ty)),
                None if REMOVED_TYPES.iter().any(|(r, _)| r == n) => return Err(unknown_type(catalog, n)),
                None => return Err(format!("unknown type `{n}` (define it above, or quote it if it is a literal)")),
            },
        }
    }
    Ok(resolved)
}

fn compile(d: &model::ModelDef, lines: &[TLine], field_types: &[Option<ScalarRef>], index: &IndexMap<&str, usize>, negation: Option<&str>) -> Result<Compiled> {
    let fields = &d.fields;
    let fidx = |n: &str| fields.iter().position(|f| f.name == n).unwrap();
    let pattern = |toks: &[Tok]| -> Pattern {
        Pattern { toks: toks.iter().filter_map(|t| match t {
            Tok::Lit(s) => Some(PTok::Lit(s.clone())),
            Tok::Hole(n) => { let i = fidx(n); Some(PTok::Hole { field: i, ty: field_types[i].clone().unwrap(), key: fields[i].kind == Kind::Key }) }
            Tok::Flag(_) | Tok::Nest(_) | Tok::Include(_) => None,
        }).collect() }
    };
    let default_value = |f: &FieldDef| -> Result<Option<Value>> {
        match (&f.default, &field_types[fidx(&f.name)]) {
            (Some(d), Some(ty)) => {
                let toks: Vec<&str> = d.iter().map(String::as_str).collect();
                let (v, n) = ty.parse(&toks).map_err(|e| Error(format!("field `{}`: invalid default `{}`: {e}", f.name, d.join(" "))))?;
                if n != toks.len() { return Err(Error(format!("field `{}`: default `{}` has extra tokens", f.name, d.join(" ")))); }
                Ok(Some(v))
            }
            _ => Ok(None),
        }
    };
    fn slot_of(l: &TLine, _siblings: &[TLine], key_prefix_len: usize, fields: &[FieldDef], field_types: &[Option<ScalarRef>], index: &IndexMap<&str, usize>, negation: Option<&str>) -> Result<Slot> {
        let fidx = |n: &str| fields.iter().position(|f| f.name == n).unwrap();
        let pattern = |toks: &[Tok]| -> Pattern {
            Pattern { toks: toks.iter().filter_map(|t| match t {
                Tok::Lit(s) => Some(PTok::Lit(s.clone())),
                Tok::Hole(n) => { let i = fidx(n); Some(PTok::Hole { field: i, ty: field_types[i].clone().unwrap(), key: fields[i].kind == Kind::Key }) }
                Tok::Flag(_) | Tok::Nest(_) | Tok::Include(_) => None, // zero-width: the line's presence is the value
            }).collect() }
        };
        let default_value = |f: &FieldDef| -> Result<Option<Value>> {
            match (&f.default, &field_types[fidx(&f.name)]) {
                (Some(d), Some(ty)) => {
                    let toks: Vec<&str> = d.iter().map(String::as_str).collect();
                    let (v, n) = ty.parse(&toks).map_err(|e| Error(format!("field `{}`: invalid default `{}`: {e}", f.name, d.join(" "))))?;
                    if n != toks.len() { return Err(Error(format!("field `{}`: default `{}` has extra tokens", f.name, d.join(" ")))); }
                    Ok(Some(v))
                }
                _ => Ok(None),
            }
        };
        if !l.children.is_empty() && l.holes().is_empty() {
            let kids: Vec<TLine> = l.children.iter().filter(|c| !c.ignore).cloned().collect();
            return Ok(Slot::Container {
                lits: pattern(&l.toks),
                body: kids.iter().filter(|c| template::absent_spelling_of(c, negation, fields).is_none() && template::negated_flag_line(c, negation, fields).is_none()).map(|c| slot_of(c, &kids, 0, fields, field_types, index, negation)).collect::<Result<_>>()?,
                ignores: l.children.iter().filter(|c| c.ignore).map(|c| c.lits()).collect(),
            });
        }
        let toks = &l.toks[key_prefix_len..];
        let values: Vec<&FieldDef> = l.holes().into_iter().map(|h| &fields[fidx(h)]).filter(|f| f.kind != Kind::Key).collect();
        match values.as_slice() {
            [f] if f.kind == Kind::Many => Ok(Slot::Nested { field: fidx(&f.name), model: index[f.type_spec.as_str()], card: Card::Many }),
            [f] if matches!(f.kind, Kind::Single { .. }) => Ok(Slot::Nested { field: fidx(&f.name), model: index[f.type_spec.as_str()], card: Card::Single { required: f.kind == Kind::Single { required: true } } }),
            [f] if f.kind == Kind::Flag => Ok(Slot::Flag {
                lits: pattern(toks),
                field: fidx(&f.name),
                default: f.default.as_ref().map(|d| d[0] == "true").unwrap_or(false),
            }),
            [f] if f.kind == Kind::Opt => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Opt }),
            [f] if f.default.is_some() => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Default(default_value(f)?.unwrap()) }),
            _ => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Required }),
        }
    }
    let slot = |l: &TLine, siblings: &[TLine], key_prefix_len: usize| -> Result<Slot> { slot_of(l, siblings, key_prefix_len, fields, field_types, index, negation) };
    let not_absent = |ls: &[TLine]| -> Vec<TLine> { ls.iter().filter(|l| template::absent_spelling_of(l, negation, fields).is_none() && template::negated_flag_line(l, negation, fields).is_none()).cloned().collect() };
    let keys: Vec<&str> = fields.iter().filter(|f| f.kind == Kind::Key).map(|f| f.name.as_str()).collect();
    let (shape, ignores) = match template::shape(lines, &keys).map_err(Error)? {
        Shape::Root { body, ignores } => (CShape::Root { body: not_absent(&body).iter().map(|l| slot(l, &body, 0)).collect::<Result<_>>()? }, ignores),
        Shape::Block { header, body, ignores } => (
            CShape::Block {
                header: pattern(&header.toks),
                key_fields: header.holes().into_iter().map(fidx).filter(|&i| fields[i].kind == Kind::Key).collect(),
                body: not_absent(&body).iter().map(|l| slot(l, &body, 0)).collect::<Result<_>>()?,
            },
            ignores,
        ),
        Shape::Flat { lines: flat, ignores } => {
            let key_len = |l: &TLine| l.toks.iter().rposition(|t| matches!(t, Tok::Hole(n) if keys.contains(&n.as_str()))).map(|p| p + 1).unwrap_or(0);
            let keys_pat = pattern(&flat[0].toks[..key_len(&flat[0])]);
            (CShape::Flat { keys: keys_pat, lines: flat.iter().map(|l| slot(l, &[], key_len(l))).collect::<Result<_>>()? }, ignores)
        }
    };
    // Defaults must parse even when the field is on a multi-value line or the header.
    for f in fields { default_value(f)?; }
    Ok(Compiled { name: d.name.clone(), fields: fields.clone(), doc: d.doc.clone(), shape, ignores, field_types: field_types.to_vec() })
}

// ---- parsing --------------------------------------------------------------------------------

enum SlotState {
    Line(Option<Vec<(usize, Value)>>),
    Flag(Option<bool>),
    Container(Option<Vec<SlotState>>),
    Block { items: Vec<Record>, keys: HashSet<Vec<Value>> },
    /// An unkeyed model used as a singleton: its body's states, and whether its line was seen.
    Inline { states: Vec<SlotState>, hit: bool },
    Flat { groups: IndexMap<Vec<Value>, (Vec<(usize, Value)>, Vec<SlotState>)> },
}

fn init_states(engine: &Engine, slots: &[Slot]) -> Vec<SlotState> {
    slots.iter().map(|s| match s {
        Slot::Line { .. } => SlotState::Line(None),
        Slot::Flag { .. } => SlotState::Flag(None),
        Slot::Nested { model, .. } => match &engine.models[*model].shape {
            CShape::Flat { .. } => SlotState::Flat { groups: IndexMap::new() },
            CShape::Block { .. } => SlotState::Block { items: Vec::new(), keys: HashSet::new() },
            CShape::Root { body } => SlotState::Inline { states: init_states(engine, body), hit: false },
        },
        Slot::Container { .. } => SlotState::Container(None),
    }).collect()
}

fn ignore_matches(prefix: &[String], toks: &[&str]) -> bool {
    prefix.len() <= toks.len() && prefix.iter().zip(toks).all(|(p, t)| p == "*" || p == t)
}

fn remnant(n: &Node<'_>, unmanaged: &mut Vec<OwnedNode>) {
    if !n.children.is_empty() { unmanaged.push(OwnedNode::from_node(n)); }
}

/// Outcome of offering one line to one slot.
enum Claim { NotMine, Claimed, Failed(String) }

impl Engine {
    pub fn parse(&self, model: &str, text: &str) -> Result<Parsed> {
        let nodes = self.dialect.lex(text);
        self.parse_nodes(model, &nodes)
    }

    pub fn parse_nodes(&self, model: &str, nodes: &[Node<'_>]) -> Result<Parsed> {
        let mi = self.model_idx(model)?;
        let m = &self.models[mi];
        let mut unmanaged = Vec::new();
        let value = match &m.shape {
            CShape::Root { body } => {
                let states = self.parse_body(body, &m.ignores, nodes, &mut unmanaged)?;
                Value::Record(self.finish(m, body, states)?)
            }
            _ => {
                // A keyed model at top level: expect exactly one instance among the nodes.
                let slots = [Slot::Nested { field: 0, model: mi, card: Card::Many }];
                let mut states = init_states(self, &slots);
                for n in nodes {
                    match self.claim(&slots[0], &mut states[0], n, &mut unmanaged) {
                        Claim::Claimed => {}
                        Claim::NotMine => {
                            if m.ignores.iter().any(|p| ignore_matches(p, &n.tokens)) { unmanaged.push(OwnedNode::from_node(n)); }
                            else if self.collides(&slots[0], n) { return Err(Error(self.unrepresentable(n))); }
                            else { unmanaged.push(OwnedNode::from_node(n)); }
                        }
                        Claim::Failed(e) => return Err(Error(e)),
                    }
                }
                let mut items = self.finish_many(m, states.pop().unwrap())?;
                if items.len() != 1 { return Err(Error(format!("expected exactly one {}, found {}", m.name, items.len()))); }
                Value::Record(items.pop().unwrap())
            }
        };
        Ok(Parsed { value, unmanaged })
    }

    fn parse_body(&self, slots: &[Slot], ignores: &[Vec<String>], nodes: &[Node<'_>], unmanaged: &mut Vec<OwnedNode>) -> Result<Vec<SlotState>> {
        let mut states = init_states(self, slots);
        // Flat and inline nested models contribute their `@ignore` prefixes to this level.
        let extra: Vec<&Vec<String>> = slots.iter().filter_map(|s| match s {
            Slot::Nested { model, .. } => match self.models[*model].shape { CShape::Flat { .. } | CShape::Root { .. } => Some(self.models[*model].ignores.iter()), _ => None },
            _ => None,
        }).flatten().collect();
        'nodes: for n in nodes {
            if ignores.iter().chain(extra.iter().copied()).any(|p| ignore_matches(p, &n.tokens)) {
                unmanaged.push(OwnedNode::from_node(n));
                continue;
            }
            for (slot, state) in slots.iter().zip(states.iter_mut()) {
                match self.claim(slot, state, n, unmanaged) {
                    Claim::Claimed => continue 'nodes,
                    Claim::Failed(e) => return Err(Error(e)),
                    Claim::NotMine => {}
                }
            }
            if slots.iter().any(|s| self.collides(s, n)) {
                return Err(Error(self.unrepresentable(n)));
            }
            unmanaged.push(OwnedNode::from_node(n));
        }
        Ok(states)
    }

    fn claim(&self, slot: &Slot, state: &mut SlotState, n: &Node<'_>, unmanaged: &mut Vec<OwnedNode>) -> Claim {
        match (slot, state) {
            (Slot::Line { pat, mode }, SlotState::Line(st)) => {
                if self.negated_bare(pat, &n.tokens) {
                    if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                    // An optional value that is explicitly negated is `null`; a defaulted one is its default.
                    *st = Some(match mode { Mode::Opt => pat.hole_fields().into_iter().map(|f| (f, Value::Null)).collect(), _ => Vec::new() });
                    remnant(n, unmanaged);
                    return Claim::Claimed;
                }
                match pat.parse_full(&n.tokens) {
                    PRes::Ok(vals, _) => {
                        if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                        *st = Some(vals);
                        remnant(n, unmanaged);
                        Claim::Claimed
                    }
                    PRes::Bad(e) => Claim::Failed(format!("`{}`: {e}", n.line_text())),
                    PRes::NoMatch => Claim::NotMine,
                }
            }
            (Slot::Container { lits, body, ignores }, SlotState::Container(st)) => match lits.parse_full(&n.tokens) {
                PRes::Ok(..) => {
                    if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                    let mut inner = Vec::new();
                    match self.parse_body(body, ignores, &n.children, &mut inner) {
                        Ok(states) => {
                            *st = Some(states);
                            if !inner.is_empty() {
                                // Keep the container in the path so the report reads like config.
                                unmanaged.push(OwnedNode { tokens: n.tokens.iter().map(|t| t.to_string()).collect(), children: inner, block: false });
                            }
                            Claim::Claimed
                        }
                        Err(e) => Claim::Failed(format!("in `{}`: {}", n.line_text(), e.0)),
                    }
                }
                _ => Claim::NotMine,
            },
            (Slot::Flag { lits, .. }, SlotState::Flag(st)) => {
                // A flag written with the negation word in the template (`no ip address`) is
                // matched literally: its only spelling is the negated one.
                let (toks, negated) = if self.literal_no(lits) { (&n.tokens[..], false) } else { self.strip_no(&n.tokens) };
                match lits.parse_full(toks) {
                    PRes::Ok(..) => {
                        if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                        *st = Some(!negated);
                        remnant(n, unmanaged);
                        Claim::Claimed
                    }
                    _ => Claim::NotMine,
                }
            }
            (Slot::Nested { model, card, .. }, SlotState::Inline { states, hit }) => {
                let m = &self.models[*model];
                let CShape::Root { body } = &m.shape else { unreachable!() };
                if *hit {
                    // A second header line of a singleton: probe with fresh state for a clear message.
                    let mut probe = init_states(self, body);
                    return match self.claim(&body[0], &mut probe[0], n, &mut Vec::new()) {
                        Claim::NotMine => Claim::NotMine,
                        _ => Claim::Failed(format!("duplicate {}: `{}` (a single {} is allowed here)", m.name, n.line_text(), m.name)),
                    };
                }
                debug_assert!(*card != Card::Many);
                let c = self.claim(&body[0], &mut states[0], n, unmanaged);
                if matches!(c, Claim::Claimed) { *hit = true; }
                c
            }
            (Slot::Nested { model, card, .. }, SlotState::Block { items, keys }) => {
                let m = &self.models[*model];
                let CShape::Block { header, key_fields, body } = &m.shape else { unreachable!() };
                match header.parse_full(&n.tokens) {
                    PRes::Ok(vals, _) => {
                        let key: Vec<Value> = key_fields.iter().map(|f| vals.iter().find(|(i, _)| i == f).unwrap().1.clone()).collect();
                        if *card != Card::Many && !items.is_empty() { return Claim::Failed(format!("duplicate {}: `{}` (a single {} is allowed here)", m.name, n.line_text(), m.name)); }
                        if !keys.insert(key) { return Claim::Failed(format!("duplicate {}: `{}`", m.name, n.line_text())); }
                        let mut inner = Vec::new();
                        let states = match self.parse_body(body, &m.ignores, &n.children, &mut inner) {
                            Ok(s) => s,
                            Err(e) => return Claim::Failed(format!("in `{}`: {}", n.line_text(), e.0)),
                        };
                        let rec = match self.finish_with(m, body, states, vals) {
                            Ok(r) => r,
                            Err(e) => return Claim::Failed(format!("in `{}`: {}", n.line_text(), e.0)),
                        };
                        items.push(rec);
                        if !inner.is_empty() {
                            unmanaged.push(OwnedNode { tokens: n.tokens.iter().map(|t| t.to_string()).collect(), children: inner, block: true });
                        }
                        Claim::Claimed
                    }
                    _ => Claim::NotMine, // a header that doesn't decode is simply not one of ours
                }
            }
            (Slot::Nested { model, card, .. }, SlotState::Flat { groups }) => {
                let m = &self.models[*model];
                let CShape::Flat { keys, lines } = &m.shape else { unreachable!() };
                let (toks, negated) = self.strip_no(&n.tokens);
                let (kvals, used) = match keys.parse_prefix(toks) {
                    PRes::Ok(v, used) => (v, used),
                    _ => return Claim::NotMine,
                };
                let rest = &toks[used..];
                // Find the claiming line first: an unclaimed line must not create an empty group
                // (a `neighbor X bfd` alone is not a neighbor).
                let mut hit: Option<(usize, Option<Vec<(usize, Value)>>)> = None;
                for (i, slot) in lines.iter().enumerate() {
                    match slot {
                        Slot::Line { pat, mode, .. } => {
                            if negated {
                                // `no neighbor X remote-as`: the negated bare form. Optional -> null, defaulted -> default.
                                if pat.has_holes() && rest == pat.literal_prefix().as_slice() {
                                    let vals = match mode { Mode::Opt => pat.hole_fields().into_iter().map(|f| (f, Value::Null)).collect(), _ => Vec::new() };
                                    hit = Some((i, Some(vals)));
                                    break;
                                }
                                continue;
                            }
                            match pat.parse_full(rest) {
                                PRes::Ok(vals, _) => { hit = Some((i, Some(vals))); break; }
                                PRes::Bad(e) => return Claim::Failed(format!("`{}`: {e}", n.line_text())),
                                PRes::NoMatch => {}
                            }
                        }
                        Slot::Flag { lits, .. } => {
                            if let PRes::Ok(..) = lits.parse_full(rest) { hit = Some((i, None)); break; }
                        }
                        _ => unreachable!(),
                    }
                }
                let Some((i, vals)) = hit else { return Claim::NotMine };
                let key: Vec<Value> = kvals.iter().map(|(_, v)| v.clone()).collect();
                if *card != Card::Many && !groups.is_empty() && !groups.contains_key(&key) {
                    return Claim::Failed(format!("duplicate {}: `{}` (a single {} is allowed here)", m.name, n.line_text(), m.name));
                }
                let entry = groups.entry(key).or_insert_with(|| (kvals.clone(), init_states(self, lines)));
                match (&mut entry.1[i], vals) {
                    (SlotState::Line(st), Some(vals)) => {
                        if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                        *st = Some(vals);
                    }
                    (SlotState::Flag(st), None) => {
                        if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                        *st = Some(!negated);
                    }
                    _ => unreachable!(),
                }
                remnant(n, unmanaged);
                Claim::Claimed
            }
            _ => unreachable!(),
        }
    }

    fn collides(&self, slot: &Slot, n: &Node<'_>) -> bool {
        match slot {
            Slot::Line { pat, .. } => {
                let (toks, negated) = self.strip_no(&n.tokens);
                pat.collides(&n.tokens) || (negated && pat.has_holes() && toks.starts_with(pat.literal_prefix().as_slice()))
            }
            Slot::Flag { lits, .. } => if self.literal_no(lits) { lits.collides(&n.tokens) } else { lits.collides(self.strip_no(&n.tokens).0) },
            Slot::Container { lits, .. } => lits.collides(&n.tokens),
            Slot::Nested { model, .. } => match &self.models[*model].shape {
                CShape::Root { body } => self.collides(&body[0], n),
                CShape::Flat { keys, lines } => {
                    let (toks, _) = self.strip_no(&n.tokens);
                    match keys.parse_prefix(toks) {
                        PRes::Ok(_, used) => lines.iter().any(|l| match l {
                            Slot::Line { pat, .. } => pat.collides(&toks[used..]) || (pat.has_holes() && toks[used..].starts_with(pat.literal_prefix().as_slice())),
                            Slot::Flag { lits, .. } => lits.collides(&toks[used..]),
                            _ => false,
                        }),
                        _ => false,
                    }
                }
                _ => false,
            },
        }
    }

    /// Build a record from the finished slot states, in field declaration order.
    fn finish(&self, m: &Compiled, slots: &[Slot], states: Vec<SlotState>) -> Result<Record> {
        let mut vals: Vec<Option<Value>> = vec![None; m.fields.len()];
        self.fill(m, slots, Some(states), &mut vals)?;
        Ok(build_record(m, vals))
    }

    /// `states == None` means the enclosing container was absent: every slot is absent.
    fn fill(&self, m: &Compiled, slots: &[Slot], states: Option<Vec<SlotState>>, vals: &mut Vec<Option<Value>>) -> Result<()> {
        let states: Vec<Option<SlotState>> = match states {
            Some(s) => s.into_iter().map(Some).collect(),
            None => slots.iter().map(|_| None).collect(),
        };
        for (slot, state) in slots.iter().zip(states) {
            match (slot, state) {
                (Slot::Line { .. }, Some(SlotState::Line(Some(vs)))) if !vs.is_empty() => { for (i, v) in vs { vals[i] = Some(v); } }
                (Slot::Line { pat, mode }, _) => match mode {
                    Mode::Required => return Err(Error(format!("required line `{}` is missing", pat.show(&m.fields)))),
                    Mode::Opt => {}
                    Mode::Default(d) => { let PTok::Hole { field, .. } = pat.toks.iter().find(|t| matches!(t, PTok::Hole { .. })).unwrap() else { unreachable!() }; vals[*field] = Some(d.clone()); }
                },
                (Slot::Flag { field, default, .. }, st) => {
                    let b = match st { Some(SlotState::Flag(Some(b))) => b, _ => *default };
                    vals[*field] = Some(Value::Bool(b));
                }
                (Slot::Nested { field, model, card: Card::Many }, st) => {
                    let items = match st { Some(st) => self.finish_many(&self.models[*model], st)?, None => Vec::new() };
                    vals[*field] = Some(Value::List(items.into_iter().map(Value::Record).collect()));
                }
                (Slot::Nested { field, model, card: Card::Single { required } }, st) => {
                    let sub = &self.models[*model];
                    let rec = match (st, &sub.shape) {
                        (Some(SlotState::Inline { states, hit: true }), CShape::Root { body }) => Some(self.finish(sub, body, states)?),
                        (Some(SlotState::Inline { .. }), _) | (None, _) => None,
                        (Some(st), _) => self.finish_many(sub, st)?.pop(),
                    };
                    match rec {
                        Some(r) => vals[*field] = Some(Value::Record(r)),
                        None if *required => return Err(Error(format!("required {} (`{}`) is missing", sub.name, m.fields[*field].name))),
                        None => {}
                    }
                }
                (Slot::Container { body, .. }, st) => {
                    let inner = match st { Some(SlotState::Container(inner)) => inner, _ => None };
                    self.fill(m, body, inner, vals)?;
                }
            }
        }
        Ok(())
    }

    /// `finish`, with header/key values merged in.
    fn finish_with(&self, m: &Compiled, slots: &[Slot], states: Vec<SlotState>, extra: Vec<(usize, Value)>) -> Result<Record> {
        let mut rec = self.finish(m, slots, states)?;
        if extra.is_empty() { return Ok(rec); }
        let mut vals: Vec<Option<Value>> = m.fields.iter().map(|f| rec.shift_remove(&f.name)).collect();
        for (i, v) in extra { vals[i] = Some(v); }
        Ok(build_record(m, vals))
    }

    fn finish_many(&self, m: &Compiled, state: SlotState) -> Result<Vec<Record>> {
        match state {
            SlotState::Block { items, .. } => Ok(items),
            SlotState::Flat { groups } => {
                let CShape::Flat { lines, .. } = &m.shape else { unreachable!() };
                let mut out = Vec::new();
                for (_, (kvals, states)) in groups {
                    let k: Vec<String> = kvals.iter().map(|(_, v)| match v { Value::Str(s) => s.clone(), Value::Int(i) => i.to_string(), v => format!("{v:?}") }).collect();
                    let rec = self.finish_with(m, lines, states, kvals).map_err(|e| Error(format!("{} {}: {}", m.name, k.join(" "), e.0)))?;
                    out.push(rec);
                }
                Ok(out)
            }
            _ => unreachable!(),
        }
    }

    // ---- rendering -----------------------------------------------------------------------

    pub fn render(&self, model: &str, value: &Value) -> Result<String> {
        Ok(self.dialect.render(&self.render_nodes(model, value)?))
    }

    pub fn render_nodes(&self, model: &str, value: &Value) -> Result<Vec<OwnedNode>> {
        let (nodes, errs) = self.render_checked(model, value)?;
        match errs.into_iter().next() { Some(e) => Err(e), None => Ok(nodes) }
    }

    /// Every problem with `value` as data for `model` (unknown or missing fields, wrong
    /// types, struct shapes, duplicate keys), in document order; empty when the data is valid.
    /// This is the render path with output discarded, so messages are the ones `render` gives.
    pub fn validate_data(&self, model: &str, value: &Value) -> Result<Vec<Error>> {
        Ok(self.render_checked(model, value)?.1)
    }

    fn render_checked(&self, model: &str, value: &Value) -> Result<(Vec<OwnedNode>, Vec<Error>)> {
        let mi = self.model_idx(model)?;
        let mut errs = Vec::new();
        let nodes = match value.as_record() {
            Some(rec) => self.render_one(&self.models[mi], rec, model, &mut errs),
            None => { errs.push(Error(format!("{model}: expected a record, got {}", value.to_json()))); Vec::new() }
        };
        Ok((nodes, errs))
    }

    /// Render one record of `m`. `path` locates it in the data (`Device.bgp[0]`); problems are
    /// pushed to `errs` and the offending part is skipped.
    fn render_one(&self, m: &Compiled, rec: &Record, path: &str, errs: &mut Vec<Error>) -> Vec<OwnedNode> {
        for k in rec.keys() {
            if !m.fields.iter().any(|f| &f.name == k) {
                errs.push(Error(format!("{path}: unknown field `{k}` ({} fields: {})", m.name, m.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", "))));
            }
        }
        match &m.shape {
            CShape::Root { body } => self.render_body(m, body, rec, path, errs),
            CShape::Block { header, body, .. } => match header.render(&m.fields, rec) {
                Ok(h) => vec![OwnedNode::with_children(h, self.render_body(m, body, rec, path, errs))],
                Err(e) => { errs.push(Error(format!("{path}: {}", e.0))); Vec::new() }
            },
            CShape::Flat { keys, lines } => {
                let prefix = match keys.render(&m.fields, rec) {
                    Ok(p) => p,
                    Err(e) => { errs.push(Error(format!("{path}: {}", e.0))); return Vec::new(); }
                };
                let neg = self.dialect.negation.as_deref();
                self.render_body(m, lines, rec, path, errs).into_iter().map(|n| {
                    // Flat lines carry the key after the negation word: `no neighbor X shutdown`.
                    let negated = neg.is_some() && n.tokens.first().map(String::as_str) == neg;
                    let mut t: Vec<String> = if negated { vec![n.tokens[0].clone()] } else { Vec::new() };
                    t.extend(prefix.iter().cloned());
                    t.extend(n.tokens[negated as usize..].iter().cloned());
                    OwnedNode::leaf(t)
                }).collect()
            }
        }
    }

    fn render_body(&self, m: &Compiled, slots: &[Slot], rec: &Record, path: &str, errs: &mut Vec<Error>) -> Vec<OwnedNode> {
        let mut out = Vec::new();
        macro_rules! fail {
            ($($arg:tt)*) => {{ errs.push(Error(format!("{path}: {}", format!($($arg)*)))); continue; }};
        }
        for slot in slots {
            match slot {
                Slot::Line { pat, mode } => {
                    let hole_fields = pat.hole_fields();
                    let present = hole_fields.iter().filter(|&&i| rec.get(&m.fields[i].name).map(|v| !v.is_null()).unwrap_or(false)).count();
                    let nulls = hole_fields.iter().filter(|&&i| rec.get(&m.fields[i].name).map(Value::is_null).unwrap_or(false)).count();
                    let negated = |neg: &String| OwnedNode::leaf(std::iter::once(neg.clone()).chain(pat.literal_prefix().iter().map(|s| s.to_string())).collect());
                    match mode {
                        Mode::Opt | Mode::Default(_) if nulls > 0 => match &self.dialect.negation {
                            // `null` means "explicitly negated": write the negated form.
                            Some(n) => { out.push(negated(n)); continue; }
                            None => fail!("field `{}`: null has no spelling in this dialect (no negation word)", m.fields[hole_fields[0]].name),
                        },
                        Mode::Opt => if present == 0 { continue; },
                        Mode::Default(d) => if present == 0 || rec.get(&m.fields[hole_fields[0]].name) == Some(d) { continue; },
                        Mode::Required => if present < hole_fields.len() {
                            let missing: Vec<&str> = hole_fields.iter().map(|&i| m.fields[i].name.as_str()).filter(|n| rec.get(*n).map(Value::is_null).unwrap_or(true)).collect();
                            fail!("required line `{}`: field(s) missing: {}", pat.show(&m.fields), missing.join(", "));
                        },
                    }
                    match pat.render(&m.fields, rec) {
                        Ok(t) => out.push(OwnedNode::leaf(t)),
                        Err(e) => fail!("{}", e.0),
                    }
                }
                Slot::Flag { lits, field, default } => {
                    let v = match rec.get(&m.fields[*field].name) {
                        None | Some(Value::Null) => *default,
                        Some(Value::Bool(b)) => *b,
                        Some(v) => fail!("field `{}`: expected true/false, got {}", m.fields[*field].name, v.to_json()),
                    };
                    if v == *default { continue; }
                    if !v && (self.dialect.negation.is_none() || self.literal_no(lits)) {
                        fail!("field `{}`: false has no spelling in this dialect (there is no negation for `{}`)", m.fields[*field].name, lits.show(&m.fields));
                    }
                    let mut toks = if v { Vec::new() } else { vec![self.dialect.negation.clone().unwrap()] };
                    match lits.render(&m.fields, rec) {
                        Ok(t) => toks.extend(t),
                        Err(e) => fail!("{}", e.0),
                    }
                    out.push(OwnedNode::leaf(toks));
                }
                Slot::Container { lits, body, .. } => {
                    let children = self.render_body(m, body, rec, path, errs);
                    if !children.is_empty() {
                        match lits.render(&m.fields, rec) {
                            Ok(t) => out.push(OwnedNode { tokens: t, children, block: false }),
                            Err(e) => fail!("{}", e.0),
                        }
                    }
                }
                Slot::Nested { field, model, card: Card::Single { required } } => {
                    let name = &m.fields[*field].name;
                    let sub = &self.models[*model];
                    match rec.get(name) {
                        None | Some(Value::Null) if *required => fail!("field `{name}` is missing (a required {})", sub.name),
                        None | Some(Value::Null) => {}
                        Some(Value::Record(r)) => out.extend(self.render_one(sub, r, &format!("{path}.{name}"), errs)),
                        Some(v) => fail!("field `{name}`: expected a record ({}), got {}", sub.name, v.to_json()),
                    }
                }
                Slot::Nested { field, model, card: Card::Many } => {
                    let name = &m.fields[*field].name;
                    let items = match rec.get(name) {
                        None | Some(Value::Null) => continue,
                        Some(Value::List(l)) => l,
                        Some(v) => fail!("field `{name}`: expected a list of {}, got {}", self.models[*model].name, v.to_json()),
                    };
                    let sub = &self.models[*model];
                    let key_fields: Vec<&str> = sub.fields.iter().filter(|f| f.kind == Kind::Key).map(|f| f.name.as_str()).collect();
                    let mut seen: IndexMap<Vec<Option<&Value>>, usize> = IndexMap::new();
                    for (i, it) in items.iter().enumerate() {
                        let at = format!("{path}.{name}[{i}]");
                        let Some(r) = it.as_record() else {
                            errs.push(Error(format!("{at}: expected a {} record, got {}", sub.name, it.to_json())));
                            continue;
                        };
                        let key: Vec<Option<&Value>> = key_fields.iter().map(|k| r.get(*k)).collect();
                        if !key_fields.is_empty() && key.iter().all(Option::is_some) {
                            if let Some(j) = seen.get(&key) {
                                errs.push(Error(format!("{at}: duplicate {} (same {} as {name}[{j}])", sub.name, key_fields.join(", "))));
                                continue;
                            }
                            seen.insert(key, i);
                        }
                        out.extend(self.render_one(sub, r, &at, errs));
                    }
                }
            }
        }
        out
    }
}

/// A field's doc as one line under its `explain` heading.
fn doc_line(f: &FieldDef, out: &mut String) {
    if let Some(doc) = &f.doc { out.push_str(&format!("  # {doc}\n")); }
}

fn fmt_value(v: &Value) -> String {
    match v { Value::Str(s) => s.clone(), Value::Int(i) => i.to_string(), Value::Bool(b) => b.to_string(), v => format!("{:?}", v.to_json()) }
}

fn build_record(m: &Compiled, vals: Vec<Option<Value>>) -> Record {
    let mut rec = Record::with_capacity(m.fields.len());
    for (f, v) in m.fields.iter().zip(vals) {
        if let Some(v) = v { rec.insert(f.name.clone(), v); }
    }
    rec
}

impl Engine {
    /// A human-readable table of how every field of `model` is spelled in config.
    pub fn explain(&self, model: &str) -> Result<String> {
        let mi = self.model_idx(model)?;
        let m = &self.models[mi];
        let mut out = String::new();
        let neg = self.dialect.negation.as_deref();
        let show = |p: &Pattern| p.toks.iter().map(|t| match t {
            PTok::Lit(s) => s.clone(),
            PTok::Hole { field, ty, .. } => format!("<{}:{}>", m.fields[*field].name, ty.describe()),
        }).collect::<Vec<_>>().join(" ");
        fn walk(e: &Engine, m: &Compiled, slots: &[Slot], prefix: &str, show: &dyn Fn(&Pattern) -> String, neg: Option<&str>, out: &mut String) {
            for slot in slots {
                match slot {
                    Slot::Line { pat, mode } => {
                        let names: Vec<&str> = pat.toks.iter().filter_map(|t| match t { PTok::Hole { field, .. } => Some(m.fields[*field].name.as_str()), _ => None }).collect();
                        let kind = match mode { Mode::Required => "required".to_string(), Mode::Opt => "optional".to_string(), Mode::Default(d) => format!("default {}", fmt_value(d)) };
                        out.push_str(&format!("{} ({kind})\n", names.join(", ")));
                        for f in pat.hole_fields() { doc_line(&m.fields[f], out); }
                        out.push_str(&format!("  value    → {prefix}{}\n", show(pat)));
                        match mode {
                            Mode::Opt => {
                                out.push_str("  missing  → (nothing written)\n");
                                if let Some(n) = neg { if pat.has_holes() { out.push_str(&format!("  null     → {n} {prefix}{}\n", pat.literal_prefix().join(" "))); } }
                            }
                            Mode::Default(_) => match neg {
                                Some(n) if pat.has_holes() => out.push_str(&format!("  default  → (nothing written; `{n} {prefix}{}` is read as the default)\n  null     → {n} {prefix}{}\n", pat.literal_prefix().join(" "), pat.literal_prefix().join(" "))),
                                _ => out.push_str("  default  → (nothing written)\n"),
                            },
                            _ => {}
                        }
                    }
                    Slot::Flag { lits, field, default } => {
                        let f = &m.fields[*field];
                        let literal_no = e.literal_no(lits);
                        out.push_str(&format!("{} (flag, default {default})\n", f.name));
                        doc_line(f, out);
                        let pos = format!("{prefix}{}", show(lits));
                        let write = |v: bool| if v == *default { "  (nothing written: default)" } else { "" };
                        if literal_no {
                            out.push_str(&format!("  true     → {pos}{}\n", write(true)));
                            out.push_str(&format!("  false    → (no spelling){}\n", write(false)));
                        } else {
                            out.push_str(&format!("  true     → {pos}{}\n", write(true)));
                            match neg {
                                Some(n) => out.push_str(&format!("  false    → {n} {prefix}{}{}\n", show(lits), write(false))),
                                None => out.push_str(&format!("  false    → (no spelling in this dialect){}\n", write(false))),
                            }
                        }
                    }
                    Slot::Nested { field, model, card: Card::Many } => {
                        out.push_str(&format!("{} (list of {})\n", m.fields[*field].name, e.models[*model].name));
                        doc_line(&m.fields[*field], out);
                        out.push_str(&format!("  each     → {prefix}one {} block/group\n", e.models[*model].name));
                    }
                    Slot::Nested { field, model, card: Card::Single { required } } => {
                        let sub = &e.models[*model];
                        out.push_str(&format!("{} (single {}, {})\n", m.fields[*field].name, sub.name, if *required { "required" } else { "optional" }));
                        doc_line(&m.fields[*field], out);
                        out.push_str(&format!("  value    → {prefix}one {} block/group (a second is an error)\n", sub.name));
                        if !*required { out.push_str("  missing  → (nothing written)\n"); }
                    }
                    Slot::Container { lits, body, .. } => {
                        walk(e, m, body, &format!("{prefix}{} > ", show(lits)), show, neg, out);
                    }
                }
            }
        }
        out.push_str(&format!("{} ({} dialect)\n", m.name, self.dialect.name));
        if let Some(doc) = &m.doc { out.push_str(&format!("# {doc}\n")); }
        match &m.shape {
            CShape::Root { body } => walk(self, m, body, "", &show, neg, &mut out),
            CShape::Block { header, key_fields, body } => {
                let keys: Vec<&str> = key_fields.iter().map(|i| m.fields[*i].name.as_str()).collect();
                out.push_str(&format!("header (identity: {})\n  → {}\n", keys.join(", "), show(header)));
                walk(self, m, body, "", &show, neg, &mut out);
            }
            CShape::Flat { keys, lines } => {
                out.push_str(&format!("flat group; every line starts with: {}\n", show(keys)));
                walk(self, m, lines, &format!("{} ", show(keys)), &show, neg, &mut out);
            }
        }
        Ok(out)
    }

    fn unrepresentable(&self, n: &Node<'_>) -> String {
        let mut msg = format!("`{}`: starts like a managed line but matches no template line (unrepresentable); model it, or add an `@ignore` line to the template to accept it", n.line_text());
        if let (Some(neg), Some(first)) = (&self.dialect.negation, n.tokens.first()) {
            if first == neg && n.tokens.len() > 1 {
                msg.push_str(&format!(". A `{neg}` line that still carries a value usually means an on/off setting: model it as a flag whose literals include the value (`… {} {{{{ field }}}}`), with the device default as its default", n.tokens[1..].join(" ")));
            }
        }
        msg
    }

    /// `no <literal prefix>` of a value line: its implicit absent form.
    fn negated_bare(&self, pat: &Pattern, toks: &[&str]) -> bool {
        let (rest, negated) = self.strip_no(toks);
        negated && pat.has_holes() && rest == pat.literal_prefix().as_slice()
    }

    /// True when a flag's literals themselves start with the negation word.
    fn literal_no(&self, lits: &Pattern) -> bool {
        matches!((&self.dialect.negation, lits.toks.first()), (Some(neg), Some(PTok::Lit(first))) if first == neg)
    }

    /// Split off the dialect's negation prefix (`no shutdown`), if it has one.
    fn strip_no<'a, 'b>(&self, toks: &'b [&'a str]) -> (&'b [&'a str], bool) {
        match (&self.dialect.negation, toks.first()) {
            (Some(neg), Some(first)) if first == neg => (&toks[1..], true),
            _ => (toks, false),
        }
    }
}
