//! Compiles `.ttp` models into patterns and slots, then parses and renders with them.
//!
//! Matching is node-major and strict: each config line is offered to the slots in template
//! order; the first full match claims it. An unclaimed line that *starts like* a managed
//! line is an error unless an `@ignore` prefix covers it; other unclaimed lines are
//! reported as unmanaged, with their ancestors, so the report reads like config.

use crate::dialect::Dialect;
use crate::lexer::{Node, OwnedNode};
use crate::model::{self, FieldDef, Kind, TypeBody};
use crate::template::{self, Shape, TLine, Tok};
use crate::types::{Catalog, EnumType, RegexType, ScalarRef};
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
    Many { field: usize, model: usize },
    /// A literal-only line with nested lines (`protocols {`, `bgp {`): its children are
    /// body lines of the same model.
    Container { lits: Pattern, body: Vec<Slot>, ignores: Vec<Vec<String>> },
}

enum CShape {
    Block { header: Pattern, key_fields: Vec<usize>, body: Vec<Slot> },
    Flat { keys: Pattern, lines: Vec<Slot> },
    Root { body: Vec<Slot> },
}

pub struct Compiled {
    pub name: String,
    pub fields: Vec<FieldDef>,
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
    /// Compile a set of parsed `.ttp` files. The dialect comes from a `dialect` section in
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
        for t in files.iter().flat_map(|f| f.types.iter()) {
            if catalog.get(&t.name).is_some() { errors.push(format!("type `{}` (line {}) is already defined", t.name, t.line)); continue; }
            match &t.def {
                TypeBody::Regex(r) => catalog.add(Arc::new(RegexType { name: t.name.clone(), source: r.clone(), re: regex::Regex::new(&format!("^(?:{r})$")).map_err(|e| Error(e.to_string()))? })),
                TypeBody::Enum(o) => catalog.add(Arc::new(EnumType { name: t.name.clone(), options: o.clone() })),
            }
        }
        let defs: Vec<&model::ModelDef> = files.iter().flat_map(|f| f.models.iter()).collect();
        let index: IndexMap<&str, usize> = defs.iter().enumerate().map(|(i, d)| (d.name.as_str(), i)).collect();
        for (i, d) in defs.iter().enumerate() {
            if index[d.name.as_str()] != i { errors.push(format!("{}:{}: model `{}` is already defined", d.source, d.line, d.name)); }
        }
        let is_keyed = |name: &str| index.get(name).map(|&i| defs[i].fields.iter().any(|f| f.kind == Kind::Key));

        let mut models: IndexMap<String, Compiled> = IndexMap::new();
        for d in &defs {
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
                    _ => match catalog.resolve(&f.type_spec) {
                        Some(t) => field_types.push(Some(t)),
                        None => { errs.push(format!("field `{}`: unknown type `{}` (known: {})", f.name, f.type_spec, catalog.names().join(", "))); field_types.push(None); }
                    },
                }
            }
            let rest = |f: &FieldDef| catalog.resolve(&f.type_spec).map(|t| t.rest_of_line()).unwrap_or(false);
            let nodes = dialect.lex_template(&d.template);
            let lines = match template::from_nodes(&nodes, d.template_line) {
                Ok(l) => l,
                Err(es) => { errs.extend(es); Vec::new() }
            };
            if !lines.is_empty() {
                errs.extend(template::validate(&d.name, &lines, &d.fields, &rest));
            }
            if !errs.is_empty() {
                errors.push(ctx(format!("\n  - {}", errs.join("\n  - "))));
                continue;
            }
            match compile(d, &lines, &field_types, &index) {
                Ok(c) => { models.insert(d.name.clone(), c); }
                Err(e) => errors.push(ctx(e.0)),
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        Ok(Engine { dialect, catalog, models })
    }

    /// Load every `*.ttp` file under `dir` (recursively). `fallback` names a builtin dialect
    /// used when the files declare none.
    pub fn load_dir(dir: &std::path::Path, fallback: Option<&str>) -> Result<Engine> {
        let mut paths = Vec::new();
        fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            for e in std::fs::read_dir(p)? {
                let e = e?.path();
                if e.is_dir() { walk(&e, out)?; } else if e.extension().map(|x| x == "ttp").unwrap_or(false) { out.push(e); }
            }
            Ok(())
        }
        walk(dir, &mut paths).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
        paths.sort();
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for p in &paths {
            let text = std::fs::read_to_string(p).map_err(|e| Error(format!("{}: {e}", p.display())))?;
            match model::parse(&p.display().to_string(), &text) {
                Ok(f) => files.push(f),
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

fn compile(d: &model::ModelDef, lines: &[TLine], field_types: &[Option<ScalarRef>], index: &IndexMap<&str, usize>) -> Result<Compiled> {
    let fields = &d.fields;
    let fidx = |n: &str| fields.iter().position(|f| f.name == n).unwrap();
    let pattern = |toks: &[Tok]| -> Pattern {
        Pattern { toks: toks.iter().map(|t| match t {
            Tok::Lit(s) => PTok::Lit(s.clone()),
            Tok::Hole(n) => { let i = fidx(n); PTok::Hole { field: i, ty: field_types[i].clone().unwrap(), key: fields[i].kind == Kind::Key } }
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
    fn slot_of(l: &TLine, key_prefix_len: usize, fields: &[FieldDef], field_types: &[Option<ScalarRef>], index: &IndexMap<&str, usize>) -> Result<Slot> {
        let fidx = |n: &str| fields.iter().position(|f| f.name == n).unwrap();
        let pattern = |toks: &[Tok]| -> Pattern {
            Pattern { toks: toks.iter().map(|t| match t {
                Tok::Lit(s) => PTok::Lit(s.clone()),
                Tok::Hole(n) => { let i = fidx(n); PTok::Hole { field: i, ty: field_types[i].clone().unwrap(), key: fields[i].kind == Kind::Key } }
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
            return Ok(Slot::Container {
                lits: pattern(&l.toks),
                body: l.children.iter().filter(|c| !c.ignore).map(|c| slot_of(c, 0, fields, field_types, index)).collect::<Result<_>>()?,
                ignores: l.children.iter().filter(|c| c.ignore).map(|c| c.lits()).collect(),
            });
        }
        let toks = &l.toks[key_prefix_len..];
        let values: Vec<&FieldDef> = l.holes().into_iter().map(|h| &fields[fidx(h)]).filter(|f| f.kind != Kind::Key).collect();
        match values.as_slice() {
            [f] if f.kind == Kind::Many => Ok(Slot::Many { field: fidx(&f.name), model: index[f.type_spec.as_str()] }),
            [f] if f.kind == Kind::Flag => Ok(Slot::Flag {
                lits: pattern(&toks.iter().filter(|t| matches!(t, Tok::Lit(_))).cloned().collect::<Vec<_>>()),
                field: fidx(&f.name),
                default: f.default.as_ref().map(|d| d[0] == "true").unwrap_or(false),
            }),
            [f] if f.kind == Kind::Opt => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Opt }),
            [f] if f.default.is_some() => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Default(default_value(f)?.unwrap()) }),
            _ => Ok(Slot::Line { pat: pattern(toks), mode: Mode::Required }),
        }
    }
    let slot = |l: &TLine, key_prefix_len: usize| -> Result<Slot> { slot_of(l, key_prefix_len, fields, field_types, index) };
    let keys: Vec<&str> = fields.iter().filter(|f| f.kind == Kind::Key).map(|f| f.name.as_str()).collect();
    let (shape, ignores) = match template::shape(lines, &keys).map_err(Error)? {
        Shape::Root { body, ignores } => (CShape::Root { body: body.iter().map(|l| slot(l, 0)).collect::<Result<_>>()? }, ignores),
        Shape::Block { header, body, ignores } => (
            CShape::Block {
                header: pattern(&header.toks),
                key_fields: header.holes().into_iter().map(fidx).filter(|&i| fields[i].kind == Kind::Key).collect(),
                body: body.iter().map(|l| slot(l, 0)).collect::<Result<_>>()?,
            },
            ignores,
        ),
        Shape::Flat { lines: flat, ignores } => {
            let key_len = |l: &TLine| l.toks.iter().rposition(|t| matches!(t, Tok::Hole(n) if keys.contains(&n.as_str()))).map(|p| p + 1).unwrap_or(0);
            let keys_pat = pattern(&flat[0].toks[..key_len(&flat[0])]);
            (CShape::Flat { keys: keys_pat, lines: flat.iter().map(|l| slot(l, key_len(l))).collect::<Result<_>>()? }, ignores)
        }
    };
    // Defaults must parse even when the field is on a multi-value line or the header.
    for f in fields { default_value(f)?; }
    Ok(Compiled { name: d.name.clone(), fields: fields.clone(), shape, ignores, field_types: field_types.to_vec() })
}

// ---- parsing --------------------------------------------------------------------------------

enum SlotState {
    Line(Option<Vec<(usize, Value)>>),
    Flag(Option<bool>),
    Container(Option<Vec<SlotState>>),
    Block { items: Vec<Record>, keys: HashSet<Vec<Value>> },
    Flat { groups: IndexMap<Vec<Value>, (Vec<(usize, Value)>, Vec<SlotState>)> },
}

fn init_states(engine: &Engine, slots: &[Slot]) -> Vec<SlotState> {
    slots.iter().map(|s| match s {
        Slot::Line { .. } => SlotState::Line(None),
        Slot::Flag { .. } => SlotState::Flag(None),
        Slot::Many { model, .. } => match &engine.models[*model].shape {
            CShape::Flat { .. } => SlotState::Flat { groups: IndexMap::new() },
            _ => SlotState::Block { items: Vec::new(), keys: HashSet::new() },
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
                let slots = [Slot::Many { field: 0, model: mi }];
                let mut states = init_states(self, &slots);
                for n in nodes {
                    match self.claim(&slots[0], &mut states[0], n, &mut unmanaged) {
                        Claim::Claimed => {}
                        Claim::NotMine => unmanaged.push(OwnedNode::from_node(n)),
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
        // Flat collections contribute their `@ignore` prefixes to this level.
        let extra: Vec<&Vec<String>> = slots.iter().filter_map(|s| match s {
            Slot::Many { model, .. } => match self.models[*model].shape { CShape::Flat { .. } => Some(self.models[*model].ignores.iter()), _ => None },
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
                return Err(Error(format!("`{}`: starts like a managed line but matches no template line (unrepresentable); model it, or add an `@ignore` line to the template to accept it", n.line_text())));
            }
            unmanaged.push(OwnedNode::from_node(n));
        }
        Ok(states)
    }

    fn claim(&self, slot: &Slot, state: &mut SlotState, n: &Node<'_>, unmanaged: &mut Vec<OwnedNode>) -> Claim {
        match (slot, state) {
            (Slot::Line { pat, .. }, SlotState::Line(st)) => match pat.parse_full(&n.tokens) {
                PRes::Ok(vals, _) => {
                    if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                    *st = Some(vals);
                    remnant(n, unmanaged);
                    Claim::Claimed
                }
                PRes::Bad(e) => Claim::Failed(format!("`{}`: {e}", n.line_text())),
                PRes::NoMatch => Claim::NotMine,
            },
            (Slot::Container { lits, body, ignores }, SlotState::Container(st)) => match lits.parse_full(&n.tokens) {
                PRes::Ok(..) => {
                    if st.is_some() { return Claim::Failed(format!("`{}`: matched twice", n.line_text())); }
                    let mut inner = Vec::new();
                    match self.parse_body(body, ignores, &n.children, &mut inner) {
                        Ok(states) => {
                            *st = Some(states);
                            if !inner.is_empty() {
                                // Keep the container in the path so the report reads like config.
                                unmanaged.push(OwnedNode { tokens: n.tokens.iter().map(|t| t.to_string()).collect(), children: inner });
                            }
                            Claim::Claimed
                        }
                        Err(e) => Claim::Failed(format!("in `{}`: {}", n.line_text(), e.0)),
                    }
                }
                _ => Claim::NotMine,
            },
            (Slot::Flag { lits, .. }, SlotState::Flag(st)) => {
                let (toks, negated) = self.strip_no(&n.tokens);
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
            (Slot::Many { model, .. }, SlotState::Block { items, keys }) => {
                let m = &self.models[*model];
                let CShape::Block { header, key_fields, body } = &m.shape else { unreachable!() };
                match header.parse_full(&n.tokens) {
                    PRes::Ok(vals, _) => {
                        let key: Vec<Value> = key_fields.iter().map(|f| vals.iter().find(|(i, _)| i == f).unwrap().1.clone()).collect();
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
                            unmanaged.push(OwnedNode { tokens: n.tokens.iter().map(|t| t.to_string()).collect(), children: inner });
                        }
                        Claim::Claimed
                    }
                    _ => Claim::NotMine, // a header that doesn't decode is simply not one of ours
                }
            }
            (Slot::Many { model, .. }, SlotState::Flat { groups }) => {
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
                        Slot::Line { pat, .. } => {
                            if negated { continue; }
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
            Slot::Line { pat, .. } => pat.collides(&n.tokens),
            Slot::Flag { lits, .. } => lits.collides(self.strip_no(&n.tokens).0),
            Slot::Container { lits, .. } => lits.collides(&n.tokens),
            Slot::Many { model, .. } => match &self.models[*model].shape {
                CShape::Flat { keys, lines } => {
                    let (toks, _) = self.strip_no(&n.tokens);
                    match keys.parse_prefix(toks) {
                        PRes::Ok(_, used) => lines.iter().any(|l| match l {
                            Slot::Line { pat, .. } => pat.collides(&toks[used..]),
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
                (Slot::Line { .. }, Some(SlotState::Line(Some(vs)))) => { for (i, v) in vs { vals[i] = Some(v); } }
                (Slot::Line { pat, mode }, _) => match mode {
                    Mode::Required => return Err(Error(format!("required line `{}` is missing", pat.show(&m.fields)))),
                    Mode::Opt => {}
                    Mode::Default(d) => { let PTok::Hole { field, .. } = pat.toks.iter().find(|t| matches!(t, PTok::Hole { .. })).unwrap() else { unreachable!() }; vals[*field] = Some(d.clone()); }
                },
                (Slot::Flag { field, default, .. }, st) => {
                    let b = match st { Some(SlotState::Flag(Some(b))) => b, _ => *default };
                    vals[*field] = Some(Value::Bool(b));
                }
                (Slot::Many { field, model }, st) => {
                    let items = match st { Some(st) => self.finish_many(&self.models[*model], st)?, None => Vec::new() };
                    vals[*field] = Some(Value::List(items.into_iter().map(Value::Record).collect()));
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
        let mi = self.model_idx(model)?;
        let rec = value.as_record().ok_or_else(|| Error(format!("{model}: expected a record")))?;
        self.render_one(&self.models[mi], rec).map_err(|e| Error(format!("{model}: {}", e.0)))
    }

    fn render_one(&self, m: &Compiled, rec: &Record) -> Result<Vec<OwnedNode>> {
        match &m.shape {
            CShape::Root { body } => self.render_body(m, body, rec),
            CShape::Block { header, body, .. } => Ok(vec![OwnedNode { tokens: header.render(&m.fields, rec)?, children: self.render_body(m, body, rec)? }]),
            CShape::Flat { keys, lines } => {
                let prefix = keys.render(&m.fields, rec)?;
                let mut out = Vec::new();
                for n in self.render_body(m, lines, rec)? {
                    let (negated, toks) = match n.tokens.first().map(String::as_str) { Some("no") => (true, &n.tokens[1..]), _ => (false, &n.tokens[..]) };
                    let mut t: Vec<String> = if negated { vec!["no".to_string()] } else { Vec::new() };
                    t.extend(prefix.iter().cloned());
                    t.extend(toks.iter().cloned());
                    out.push(OwnedNode::leaf(t));
                }
                Ok(out)
            }
        }
    }

    fn render_body(&self, m: &Compiled, slots: &[Slot], rec: &Record) -> Result<Vec<OwnedNode>> {
        let mut out = Vec::new();
        for slot in slots {
            match slot {
                Slot::Line { pat, mode } => {
                    let hole_fields: Vec<usize> = pat.toks.iter().filter_map(|t| match t { PTok::Hole { field, .. } => Some(*field), _ => None }).collect();
                    let present = hole_fields.iter().filter(|&&i| rec.get(&m.fields[i].name).map(|v| !v.is_null()).unwrap_or(false)).count();
                    match mode {
                        Mode::Opt => { if present == 0 { continue; } }
                        Mode::Default(d) => { if present == 0 || rec.get(&m.fields[hole_fields[0]].name) == Some(d) { continue; } }
                        Mode::Required => { if present < hole_fields.len() { return Err(Error(format!("required line `{}`: field(s) missing", pat.show(&m.fields)))); } }
                    }
                    out.push(OwnedNode::leaf(pat.render(&m.fields, rec)?));
                }
                Slot::Flag { lits, field, default } => {
                    let v = match rec.get(&m.fields[*field].name) {
                        None | Some(Value::Null) => *default,
                        Some(Value::Bool(b)) => *b,
                        Some(v) => return Err(Error(format!("field `{}`: expected true/false, got {v:?}", m.fields[*field].name))),
                    };
                    if v == *default { continue; }
                    let mut toks = if v { Vec::new() } else { vec!["no".to_string()] };
                    toks.extend(lits.render(&m.fields, rec)?);
                    out.push(OwnedNode::leaf(toks));
                }
                Slot::Container { lits, body, .. } => {
                    let children = self.render_body(m, body, rec)?;
                    if !children.is_empty() {
                        out.push(OwnedNode { tokens: lits.render(&m.fields, rec)?, children });
                    }
                }
                Slot::Many { field, model } => {
                    let items = match rec.get(&m.fields[*field].name) {
                        None | Some(Value::Null) => continue,
                        Some(Value::List(l)) => l,
                        Some(v) => return Err(Error(format!("field `{}`: expected a list, got {v:?}", m.fields[*field].name))),
                    };
                    let sub = &self.models[*model];
                    for it in items {
                        let r = it.as_record().ok_or_else(|| Error(format!("field `{}`: expected records", m.fields[*field].name)))?;
                        out.extend(self.render_one(sub, r).map_err(|e| Error(format!("{}: {}", sub.name, e.0)))?);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn build_record(m: &Compiled, vals: Vec<Option<Value>>) -> Record {
    let mut rec = Record::with_capacity(m.fields.len());
    for (f, v) in m.fields.iter().zip(vals) {
        if let Some(v) = v { rec.insert(f.name.clone(), v); }
    }
    rec
}

impl Engine {
    /// Split off the dialect's negation prefix (`no shutdown`), if it has one.
    fn strip_no<'a, 'b>(&self, toks: &'b [&'a str]) -> (&'b [&'a str], bool) {
        match (&self.dialect.negation, toks.first()) {
            (Some(neg), Some(first)) if first == neg => (&toks[1..], true),
            _ => (toks, false),
        }
    }
}
