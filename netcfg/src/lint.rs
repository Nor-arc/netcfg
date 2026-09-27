//! `netcfg lint`: warnings about a set that loads but may not do what its author meant.
//!
//! Always:
//! - **shadowed lines**: a template line after one with the same literal prefix that claims
//!   (or rejects) every line starting that way, because lines are offered in template order;
//! - **ambiguous claims**: nested models, or a nested model and a line, at the same level whose
//!   entry lines share a literal prefix.
//!
//! With golden configs (`--golden dir`):
//! - models that never appear, and value fields that are never set;
//! - flags that are always at their declared default (is the default right? is it needed?);
//! - `@ignore` prefixes that match no line.

use crate::engine::{ignore_matches, CShape, Compiled, Engine, PTok, Pattern, Slot};
use crate::lexer::Node;
use crate::model::Kind;
use crate::value::{Record, Value};
use std::path::PathBuf;

/// One warning: where, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub at: String,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// Golden configs for lint: the device model and `(path, text)` pairs.
pub struct Goldens {
    pub model: String,
    pub configs: Vec<(PathBuf, String)>,
}

fn show(p: &Pattern, m: &Compiled) -> String {
    p.toks.iter().map(|t| match t {
        PTok::Lit(s) => s.clone(),
        PTok::Hole { field, .. } => format!("{{{{ {} }}}}", m.fields[*field].name),
    }).collect::<Vec<_>>().join(" ")
}

/// A line that claims every line starting with its literal prefix: nothing but placeholders
/// after the prefix.
fn greedy(p: &Pattern) -> bool {
    let n = p.literal_prefix().len();
    p.toks[n..].iter().all(|t| matches!(t, PTok::Hole { .. }))
}

impl Engine {
    pub fn lint(&self, goldens: Option<&Goldens>) -> Vec<Warning> {
        let mut out = Vec::new();
        for m in self.models.values() {
            let at = |p: &Pattern| format!("{}: model {}: {}", m.source, m.name, p.at);
            match &m.shape {
                CShape::Root { body } => self.lint_body(m, body, &[], &at, &mut out),
                CShape::Block { body, .. } => self.lint_body(m, body, &[], &at, &mut out),
                CShape::Flat { keys, lines } => self.lint_body(m, lines, &keys.literal_prefix(), &at, &mut out),
            }
        }
        if let Some(g) = goldens { self.lint_goldens(g, &mut out); }
        out
    }

    /// The literal prefix a nested model's entry line starts with, and where it is.
    fn entry<'a>(&'a self, sub: &'a Compiled) -> Option<(Vec<&'a str>, &'a Pattern)> {
        match &sub.shape {
            CShape::Block { header, .. } => Some((header.literal_prefix(), header)),
            CShape::Flat { keys, .. } => Some((keys.literal_prefix(), keys)),
            CShape::Root { body } => match body.first() {
                Some(Slot::Line { pat, .. }) => Some((pat.literal_prefix(), pat)),
                Some(Slot::Flag { lits, .. }) | Some(Slot::Container { lits, .. }) => Some((lits.literal_prefix(), lits)),
                _ => None,
            },
        }
    }

    fn lint_body(&self, m: &Compiled, slots: &[Slot], flat_prefix: &[&str], at: &dyn Fn(&Pattern) -> String, out: &mut Vec<Warning>) {
        // Shadowed lines.
        let lines: Vec<(&Pattern, bool)> = slots.iter().filter_map(|s| match s {
            Slot::Line { pat, .. } => Some((pat, greedy(pat))),
            Slot::Flag { lits, .. } => Some((lits, false)),
            _ => None,
        }).collect();
        for (j, (b, _)) in lines.iter().enumerate() {
            for (a, a_greedy) in &lines[..j] {
                let pa = a.literal_prefix();
                let same = show(a, m) == show(b, m);
                if pa == b.literal_prefix() && (*a_greedy || same) && (!pa.is_empty() || same) {
                    let pre: Vec<&str> = flat_prefix.iter().copied().chain(pa.iter().copied()).collect();
                    out.push(Warning { at: at(b), message: format!("`{}` is shadowed by `{}` ({}): lines are offered to template lines in order, and that line claims (or rejects) every line starting with `{}`", show(b, m), show(a, m), a.at, pre.join(" ")) });
                    break;
                }
            }
        }
        // Ambiguous claims between nested models, and between a nested model and a line.
        let nested: Vec<(&str, &Compiled, Vec<&str>, &Pattern)> = slots.iter().filter_map(|s| match s {
            Slot::Nested { field, model, .. } => {
                let sub = &self.models[*model];
                self.entry(sub).map(|(p, pat)| (m.fields[*field].name.as_str(), sub, p, pat))
            }
            _ => None,
        }).collect();
        for (j, (fb, sb, pb, _)) in nested.iter().enumerate() {
            for (fa, sa, pa, _) in &nested[..j] {
                if !pa.is_empty() && pa == pb && sa.name != sb.name {
                    out.push(Warning { at: format!("{}: model {}", m.source, m.name), message: format!("<< {fa} >> ({}) and << {fb} >> ({}) both claim lines starting with `{}`; the first in template order wins", sa.name, sb.name, pa.join(" ")) });
                }
            }
            for (l, _) in &lines {
                if !pb.is_empty() && l.literal_prefix() == *pb {
                    out.push(Warning { at: at(l), message: format!("`{}` and << {fb} >> ({}) both claim lines starting with `{}`", show(l, m), sb.name, pb.join(" ")) });
                }
            }
        }
        for s in slots {
            if let Slot::Container { body, .. } = s { self.lint_body(m, body, &[], at, out); }
        }
    }

    fn lint_goldens(&self, g: &Goldens, out: &mut Vec<Warning>) {
        let Some(root) = self.model(&g.model) else {
            out.push(Warning { at: "goldens".into(), message: format!("unknown device model `{}`", g.model) });
            return;
        };
        let mut parsed = Vec::new();
        for (path, text) in &g.configs {
            match self.parse(&g.model, text) {
                Ok(p) => parsed.push(p.value),
                Err(e) => out.push(Warning { at: path.display().to_string(), message: format!("golden config does not parse (run `netcfg check`): {}", e.0) }),
            }
        }
        let mut records: indexmap::IndexMap<&str, Vec<&Record>> = self.reachable(root).into_iter().map(|n| (n, Vec::new())).collect();
        for v in &parsed {
            if let Some(r) = v.as_record() {
                self.walk_data(root, r, &root.name, &mut |m, r, _| { if let Some(list) = records.get_mut(m.name.as_str()) { list.push(r); } });
            }
        }
        for (name, recs) in &records {
            let m = &self.models[*name];
            if recs.is_empty() {
                out.push(Warning { at: m.declared.clone(), message: format!("model {name} never appears in the {} golden config(s)", parsed.len()) });
                continue;
            }
            for (i, f) in m.fields.iter().enumerate() {
                let file = m.declared.rsplit_once(':').map(|(f, _)| f).unwrap_or(&m.declared);
                let at = format!("{file}:{}: model {name}: field `{}`", f.line, f.name);
                let default = self.field_default(m, i);
                match f.kind {
                    Kind::Key | Kind::Many | Kind::Single { .. } => {}
                    Kind::Scalar if f.default.is_none() => {}
                    Kind::Flag => {
                        if recs.iter().all(|r| r.get(&f.name) == default.as_ref()) {
                            out.push(Warning { at, message: format!("flag is {} (its declared default) in all {} record(s) of the goldens; is the default the device's, and is the flag needed?", default.map(|d| d.to_json().to_string()).unwrap_or_default(), recs.len()) });
                        }
                    }
                    _ => {
                        let set = |v: Option<&Value>| match v { None => false, Some(v) => Some(v) != default.as_ref() };
                        if !recs.iter().any(|r| set(r.get(&f.name))) {
                            out.push(Warning { at, message: format!("never set in the goldens ({} record(s))", recs.len()) });
                        }
                    }
                }
            }
        }
        // @ignore prefixes that match nothing.
        let lexed: Vec<Vec<Node<'_>>> = g.configs.iter().map(|(_, t)| self.dialect.lex(t)).collect();
        fn any_match(nodes: &[Node<'_>], p: &[String]) -> bool {
            nodes.iter().any(|n| ignore_matches(p, &n.tokens) || any_match(&n.children, p))
        }
        for m in self.models.values() {
            for (prefix, at) in &m.ignore_at {
                if !lexed.iter().any(|ns| any_match(ns, prefix)) {
                    out.push(Warning { at: format!("{}: model {}: {at}", m.source, m.name), message: format!("`@ignore {}` matches no line in the goldens", prefix.join(" ")) });
                }
            }
        }
    }
}
