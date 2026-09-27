//! Change sets: the commands that take a running config (as parsed data) to intent data.
//!
//! Both sides are walked by model. Keyed collections match elements by key; positional
//! collections are replaced whole when anything differs; singletons compare presence. An
//! added block is rendered in full, a removed one is `<delete> <header>`, and a changed one is
//! entered by its header with the changed lines inside. For a value field:
//!
//! | running → intent | command |
//! |---|---|
//! | same | nothing |
//! | anything → other value | the value line |
//! | value or missing → `null` | the negated form (`no remote-as`) |
//! | anything → key missing | nothing (no opinion), unless `explicit` |
//!
//! Flags are compared with their defaults applied and written positive or negated. With
//! `explicit`, intent is the complete desired state: a missing value is cleared (negated),
//! a missing flag or defaulted field is its default, and a missing collection is empty.

use crate::dialect::{quote, Dialect, Grammar};
use crate::engine::{CShape, Card, Compiled, Engine, Mode, Pattern, RenderMode, Slot};
use crate::lexer::OwnedNode;
use crate::model::Kind;
use indexmap::IndexMap;
use crate::value::{Record, Value};
use crate::{Error, Result};

/// One step of a change set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Enter a block by its header to change things inside it. `was` is the running header
    /// when the header itself changes (a value on the header line, like a route-map action).
    Enter { line: Vec<String>, was: Option<Vec<String>>, children: Vec<Change> },
    /// Write a line (or a whole new block). `was` is the running line it replaces, if any.
    Set { node: OwnedNode, was: Option<Vec<String>> },
    /// Remove a statement or block. `group` removes every line starting with `line` (a flat
    /// group, `no neighbor 10.0.0.1`); otherwise the statement whose line is exactly `line`.
    Delete { line: Vec<String>, group: bool },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiffOptions {
    /// Intent is the complete desired state: missing keys mean absent/default/empty.
    pub explicit: bool,
}

/// The ordered commands that take running to intent, in the dialect's syntax.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
    dialect: Dialect,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Config text ready to paste: indented statements with `no`, or Junos `set`/`delete`.
    pub fn to_text(&self) -> Result<String> {
        let d = &self.dialect;
        match d.grammar {
            Grammar::Indent => {
                let del = d.delete_word().ok_or_else(|| Error(format!("dialect {} has no delete word (declare `delete:` or `negation:`)", d.name)))?;
                fn conv(cs: &[Change], del: &str) -> Vec<OwnedNode> {
                    cs.iter().map(|c| match c {
                        Change::Enter { line, children, .. } => OwnedNode { tokens: line.clone(), children: conv(children, del), block: true },
                        Change::Set { node, .. } => node.clone(),
                        Change::Delete { line, .. } => OwnedNode::leaf(std::iter::once(del.to_string()).chain(line.iter().cloned()).collect()),
                    }).collect()
                }
                let mut quiet = d.clone();
                quiet.end_marker = None;
                Ok(quiet.render(&conv(&self.changes, del)))
            }
            Grammar::Braces => {
                let del = d.delete.as_deref().unwrap_or("delete");
                let mut out = String::new();
                for op in self.ops() {
                    let words: Vec<String> = op.path.iter().chain(std::iter::once(&op.line)).flat_map(|l| l.iter().map(|t| quote(t))).collect();
                    out.push_str(if op.delete { del } else { "set" });
                    out.push(' ');
                    out.push_str(&words.join(" "));
                    out.push('\n');
                }
                Ok(out)
            }
        }
    }

    /// Flattened operations: each with its enclosing block headers.
    pub fn ops(&self) -> Vec<Op> {
        fn leaves(n: &OwnedNode, path: &mut Vec<Vec<String>>, was: Option<&Vec<String>>, out: &mut Vec<Op>) {
            if n.children.is_empty() {
                out.push(Op { delete: false, path: path.clone(), line: n.tokens.clone(), was: was.cloned() });
                return;
            }
            path.push(n.tokens.clone());
            for c in &n.children { leaves(c, path, None, out); }
            path.pop();
        }
        fn go(cs: &[Change], path: &mut Vec<Vec<String>>, out: &mut Vec<Op>) {
            for c in cs {
                match c {
                    Change::Enter { line, was, children } => {
                        // A changed header is itself an edit (`route-map RM permit 10` -> `deny 10`).
                        if was.is_some() { out.push(Op { delete: false, path: path.clone(), line: line.clone(), was: was.clone() }); }
                        path.push(line.clone());
                        go(children, path, out);
                        path.pop();
                    }
                    Change::Set { node, was } => leaves(node, path, was.as_ref(), out),
                    Change::Delete { line, .. } => out.push(Op { delete: true, path: path.clone(), line: line.clone(), was: None }),
                }
            }
        }
        let mut out = Vec::new();
        go(&self.changes, &mut Vec::new(), &mut out);
        out
    }

    /// `[{"op": "set"|"delete", "path": [...], "line": "...", "was": "..."}]`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.ops().into_iter().map(|o| {
            let mut m = serde_json::Map::new();
            m.insert("op".into(), (if o.delete { "delete" } else { "set" }).into());
            m.insert("path".into(), o.path.iter().map(|l| serde_json::Value::String(l.join(" "))).collect());
            m.insert("line".into(), o.line.join(" ").into());
            if let Some(w) = o.was { m.insert("was".into(), w.join(" ").into()); }
            serde_json::Value::Object(m)
        }).collect())
    }
}

/// One flattened operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub delete: bool,
    /// Enclosing block headers, outermost first.
    pub path: Vec<Vec<String>>,
    pub line: Vec<String>,
    pub was: Option<Vec<String>>,
}

/// A field's state in one record.
#[derive(Clone, Copy, PartialEq)]
enum St<'a> {
    Missing,
    Null,
    Val(&'a Value),
}

fn st<'a>(rec: &'a Record, name: &str) -> St<'a> {
    match rec.get(name) {
        None => St::Missing,
        Some(Value::Null) => St::Null,
        Some(v) => St::Val(v),
    }
}

/// The identity of an element: its key field values.
fn key_of(m: &Compiled, rec: &Record) -> Vec<Option<Value>> {
    m.fields.iter().filter(|f| f.kind == Kind::Key).map(|f| rec.get(&f.name).cloned()).collect()
}

fn leaf(tokens: Vec<String>) -> OwnedNode {
    OwnedNode::leaf(tokens)
}

impl Engine {
    /// The change set that takes `running` to `intent` (both data for `model`). Intent is
    /// validated first (every error is reported). Structured braces rendering has no deletion
    /// form, so it is refused: declare `render: set`.
    pub fn diff(&self, model: &str, running: &Value, intent: &Value) -> Result<ChangeSet> {
        self.diff_with(model, running, intent, DiffOptions::default())
    }

    pub fn diff_with(&self, model: &str, running: &Value, intent: &Value, opts: DiffOptions) -> Result<ChangeSet> {
        if self.dialect.grammar == Grammar::Braces && !self.dialect.render_set {
            return Err(Error(format!("dialect {}: structured braces rendering has no deletion form; `diff` needs `render: set`", self.dialect.name)));
        }
        for (what, v) in [("intent", intent), ("running", running)] {
            let errs = self.validate_data(model, v)?;
            if !errs.is_empty() {
                return Err(Error(format!("{what} data is invalid:\n  {}", errs.iter().map(|e| e.0.as_str()).collect::<Vec<_>>().join("\n  "))));
            }
        }
        let m = &self.models[model];
        let (r, i) = (running.as_record().unwrap(), intent.as_record().unwrap());
        let changes = self.diff_item(m, Some(r), Some(i), opts)?;
        Ok(ChangeSet { changes, dialect: self.dialect.clone() })
    }

    /// Changes for one element of `m` at its parent's level.
    fn diff_item(&self, m: &Compiled, r: Option<&Record>, i: Option<&Record>, opts: DiffOptions) -> Result<Vec<Change>> {
        let render = |rec: &Record| -> Result<Vec<OwnedNode>> {
            let mut errs = Vec::new();
            let nodes = self.render_one(m, rec, &m.name, RenderMode::Canonical, &mut errs);
            match errs.into_iter().next() { Some(e) => Err(e), None => Ok(nodes) }
        };
        match (r, i) {
            (None, None) => Ok(Vec::new()),
            (None, Some(i)) => Ok(render(i)?.into_iter().map(|node| Change::Set { node, was: None }).collect()),
            (Some(r), None) => Ok(match &m.shape {
                CShape::Block { header, .. } => vec![Change::Delete { line: header.render(&m.fields, r)?, group: false }],
                CShape::Flat { keys, .. } => vec![Change::Delete { line: keys.render(&m.fields, r)?, group: true }],
                // An unkeyed element is identified by its one top-level line.
                CShape::Root { .. } => render(r)?.into_iter().map(|n| Change::Delete { line: n.tokens, group: false }).collect(),
            }),
            (Some(r), Some(i)) => match &m.shape {
                CShape::Root { body } => self.diff_body(m, body, r, i, opts),
                CShape::Block { header, body, .. } => {
                    let children = self.diff_body(m, body, r, i, opts)?;
                    let (hr, hi) = (header.render(&m.fields, r)?, header.render(&m.fields, i)?);
                    if children.is_empty() && hr == hi { return Ok(Vec::new()); }
                    let was = if hr != hi { Some(hr) } else { None };
                    Ok(vec![Change::Enter { line: hi, was, children }])
                }
                CShape::Flat { keys, lines } => {
                    let prefix = keys.render(&m.fields, i)?;
                    let neg = self.dialect.negation.as_deref();
                    let with_key = |t: Vec<String>| -> Vec<String> {
                        let negated = neg.is_some() && t.first().map(String::as_str) == neg;
                        let mut out: Vec<String> = if negated { vec![t[0].clone()] } else { Vec::new() };
                        out.extend(prefix.iter().cloned());
                        out.extend(t[negated as usize..].iter().cloned());
                        out
                    };
                    Ok(self.diff_body(m, lines, r, i, opts)?.into_iter().map(|c| match c {
                        Change::Set { node, was } => Change::Set { node: leaf(with_key(node.tokens)), was: was.map(with_key) },
                        Change::Delete { line, group } => Change::Delete { line: with_key(line), group },
                        enter => enter,
                    }).collect())
                }
            },
        }
    }

    /// Changes for the body lines of `m`: removals first, then additions and edits, each in
    /// template order.
    fn diff_body(&self, m: &Compiled, slots: &[Slot], r: &Record, i: &Record, opts: DiffOptions) -> Result<Vec<Change>> {
        let mut dels: Vec<Change> = Vec::new();
        let mut sets: Vec<Change> = Vec::new();
        let neg = self.dialect.negation.clone();
        let negated = |pat: &Pattern| -> Option<Vec<String>> {
            neg.as_ref().map(|n| std::iter::once(n.clone()).chain(pat.literal_prefix().iter().map(|s| s.to_string())).collect())
        };
        for slot in slots {
            match slot {
                Slot::Line { pat, mode } => {
                    let holes = pat.hole_fields();
                    let name = |k: usize| m.fields[holes[k]].name.as_str();
                    // The line canonical rendering writes for a record, if any.
                    let line_of = |rec: &Record| -> Result<Option<Vec<String>>> {
                        match (mode, st(rec, name(0))) {
                            (Mode::Required, _) => pat.render(&m.fields, rec).map(Some),
                            (_, St::Missing) => Ok(None),
                            (_, St::Null) => Ok(negated(pat)),
                            (Mode::Default(d), St::Val(v)) if v == d => Ok(None),
                            (_, St::Val(_)) => pat.render(&m.fields, rec).map(Some),
                        }
                    };
                    if let Mode::Required = mode {
                        if holes.iter().any(|&h| r.get(&m.fields[h].name) != i.get(&m.fields[h].name)) {
                            sets.push(Change::Set { node: leaf(pat.render(&m.fields, i)?), was: Some(pat.render(&m.fields, r)?) });
                        }
                        continue;
                    }
                    let default = match mode { Mode::Default(d) => Some(d), _ => None };
                    // Effective values: a defaulted field that is missing is its default.
                    let eff = |s: St<'_>| -> Option<Value> {
                        match s { St::Val(v) => Some(v.clone()), St::Null => None, St::Missing => default.cloned() }
                    };
                    let (rs, is) = (st(r, name(0)), st(i, name(0)));
                    let was = line_of(r)?;
                    match is {
                        St::Missing if !opts.explicit => {}
                        St::Missing | St::Null => {
                            // Clear it: the negated form, unless running is already clear.
                            let already = match (is, mode) {
                                (St::Null, _) => rs == St::Null,
                                (_, Mode::Default(_)) => eff(rs) == default.cloned(),
                                _ => matches!(rs, St::Missing | St::Null),
                            };
                            if !already {
                                match negated(pat) {
                                    Some(n) => sets.push(Change::Set { node: leaf(n), was }),
                                    None => if let Some(w) = was { dels.push(Change::Delete { line: w, group: false }); },
                                }
                            }
                        }
                        St::Val(v) => {
                            if eff(St::Val(v)) == eff(rs) && rs != St::Null { continue; }
                            if Some(v) == default && negated(pat).is_some() {
                                // Back to the default: the negated form resets it.
                                sets.push(Change::Set { node: leaf(negated(pat).unwrap()), was });
                            } else {
                                sets.push(Change::Set { node: leaf(pat.render(&m.fields, i)?), was });
                            }
                        }
                    }
                }
                Slot::Flag { lits, field, default } => {
                    let fname = &m.fields[*field].name;
                    let get = |rec: &Record| match rec.get(fname) { Some(Value::Bool(b)) => Some(*b), Some(Value::Null) => Some(*default), _ => None };
                    let rv = get(r).unwrap_or(*default);
                    let iv = match get(i) { Some(b) => b, None if opts.explicit => *default, None => continue };
                    if iv == rv { continue; }
                    let positive = lits.render(&m.fields, i)?;
                    let literal_no = self.literal_no(lits);
                    // The spelling of a flag value, if it has one.
                    let spell = |v: bool| -> Option<Vec<String>> {
                        match (v, literal_no) {
                            (true, _) => Some(positive.clone()),
                            (false, true) => Some(positive[1..].to_vec()),
                            (false, false) => neg.as_ref().map(|n| std::iter::once(n.clone()).chain(positive.iter().cloned()).collect()),
                        }
                    };
                    let was = if rv != *default { spell(rv) } else { None };
                    match spell(iv) {
                        Some(line) => sets.push(Change::Set { node: leaf(line), was }),
                        None => match was {
                            Some(w) => dels.push(Change::Delete { line: w, group: false }),
                            None => return Err(Error(format!("field `{fname}`: {iv} has no spelling in this dialect"))),
                        },
                    }
                }
                Slot::Container { lits, body, .. } => {
                    let children = self.diff_body(m, body, r, i, opts)?;
                    if !children.is_empty() {
                        sets.push(Change::Enter { line: lits.render(&m.fields, i)?, was: None, children });
                    }
                }
                Slot::Nested { field, model, card } => {
                    let name = &m.fields[*field].name;
                    let sub = &self.models[*model];
                    match card {
                        Card::Many | Card::Ordered => {
                            let list = |rec: &Record| -> Option<Vec<Record>> {
                                match rec.get(name) {
                                    None => None,
                                    Some(Value::List(l)) => Some(l.iter().filter_map(|x| x.as_record().cloned()).collect()),
                                    Some(_) => Some(Vec::new()),
                                }
                            };
                            let ri = list(r).unwrap_or_default();
                            let ii = match list(i) { Some(l) => l, None if opts.explicit => Vec::new(), None => continue };
                            if *card == Card::Ordered {
                                // Positional: replace the whole collection when anything differs.
                                if ri == ii { continue; }
                                for x in &ri { dels.extend(self.diff_item(sub, Some(x), None, opts)?); }
                                for x in &ii { sets.extend(self.diff_item(sub, None, Some(x), opts)?); }
                                continue;
                            }
                            let by_key: IndexMap<Vec<Option<Value>>, &Record> = ri.iter().map(|x| (key_of(sub, x), x)).collect();
                            let wanted: std::collections::HashSet<Vec<Option<Value>>> = ii.iter().map(|y| key_of(sub, y)).collect();
                            for x in &ri {
                                if !wanted.contains(&key_of(sub, x)) { dels.extend(self.diff_item(sub, Some(x), None, opts)?); }
                            }
                            for y in &ii {
                                sets.extend(self.diff_item(sub, by_key.get(&key_of(sub, y)).copied(), Some(y), opts)?);
                            }
                        }
                        Card::Single { .. } => {
                            let one = |rec: &Record| -> Option<Option<Record>> {
                                match rec.get(name) { None => None, Some(Value::Record(x)) => Some(Some(x.clone())), Some(_) => Some(None) }
                            };
                            let rx = one(r).flatten();
                            let ix = match one(i) { Some(x) => x, None if opts.explicit => None, None => continue };
                            let keyed = !matches!(sub.shape, CShape::Root { .. });
                            match (&rx, &ix) {
                                (Some(a), Some(b)) if keyed && key_of(sub, a) != key_of(sub, b) => {
                                    dels.extend(self.diff_item(sub, Some(a), None, opts)?);
                                    sets.extend(self.diff_item(sub, None, Some(b), opts)?);
                                }
                                (Some(_), None) => dels.extend(self.diff_item(sub, rx.as_ref(), None, opts)?),
                                _ => sets.extend(self.diff_item(sub, rx.as_ref(), ix.as_ref(), opts)?),
                            }
                        }
                    }
                }
            }
        }
        dels.extend(sets);
        Ok(dels)
    }
}
