//! `netcfg skeleton`: example YAML data for a model, with every field present, typed
//! placeholders and comments from the declarations.

use crate::engine::{Compiled, Engine};
use crate::model::{FieldDef, Kind};
use crate::types::ScalarRef;

/// `int(576..9216)` reads better as `int 576..9216` in a placeholder.
fn spell(d: &str) -> String {
    match d.strip_prefix("int(").and_then(|r| r.strip_suffix(')')) {
        Some(range) => format!("int {range}"),
        None => d.to_string(),
    }
}

/// `<asn>`, `<int 576..9216>`, `[<string>]`; an anonymous union shows its alternatives
/// (without `""`, which the comment spells as "may be omitted").
fn placeholder(ty: &ScalarRef) -> String {
    if let Some(elem) = ty.elem() { return format!("[{}]", placeholder(elem)); }
    let d = ty.describe();
    if d.contains('.') {
        let alts = ty.hint().unwrap_or(d);
        let alts: Vec<String> = alts.split(" | ").filter(|a| *a != "\"\"").map(spell).collect();
        return format!("<{}>", alts.join(" | "));
    }
    format!("<{}>", spell(&d))
}

fn comment(parts: &[String]) -> String {
    let parts: Vec<&str> = parts.iter().map(String::as_str).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { String::new() } else { format!("  # {}", parts.join("; ")) }
}

impl Engine {
    /// Example YAML for `model`: every field present with a typed placeholder, comments from
    /// docs and types, and one example element per collection.
    pub fn skeleton(&self, model: &str) -> crate::Result<String> {
        let m = self.model(model).ok_or_else(|| crate::Error(format!("unknown model `{model}` (known: {})", self.model_names().join(", "))))?;
        let mut out = format!("# {model} ({} dialect): every field, with placeholders to replace.\n", self.dialect.name);
        if let Some(doc) = &m.doc { out.push_str(&format!("# {doc}\n")); }
        match &self.dialect.negation {
            Some(n) => out.push_str(&format!("# A value field: leave the key out to write nothing, give a value to write it, or\n# null to write its negated form (`{n} ...`), which clears it on the device.\n")),
            None => out.push_str("# A value field: leave the key out to write nothing, or give a value to write it.\n"),
        }
        out.push_str("# A flag is true/false and is written only when it differs from its default.\n# Collections show one example element.\n");
        let mut stack = vec![m.name.clone()];
        self.skeleton_fields(m, 0, false, &mut stack, &mut out);
        Ok(out)
    }

    /// Write `m`'s fields at `indent`. `item` puts `- ` before the first field (a list element).
    fn skeleton_fields(&self, m: &Compiled, indent: usize, item: bool, stack: &mut Vec<String>, out: &mut String) {
        for (i, f) in m.fields.iter().enumerate() {
            let pad = if item && i == 0 { format!("{}- ", " ".repeat(indent)) } else { " ".repeat(indent + if item { 2 } else { 0 }) };
            let inner = indent + if item { 2 } else { 0 } + 2;
            self.skeleton_field(m, i, f, &pad, inner, stack, out);
        }
        if m.fields.is_empty() && item { out.push_str(&format!("{}- {{}}\n", " ".repeat(indent))); }
    }

    #[allow(clippy::too_many_arguments)]
    fn skeleton_field(&self, m: &Compiled, i: usize, f: &FieldDef, pad: &str, inner: usize, stack: &mut Vec<String>, out: &mut String) {
        let doc = f.doc.clone().unwrap_or_default();
        let kind = match f.kind {
            Kind::Key => "key".to_string(),
            Kind::Scalar if f.default.is_some() => "default".to_string(),
            Kind::Scalar => "required".to_string(),
            Kind::Opt => "optional".to_string(),
            Kind::Flag => format!("flag, default {}", f.default.as_ref().map(|d| d[0].as_str()).unwrap_or("false")),
            Kind::Many => format!("list of {}", f.type_spec),
            Kind::Single { required } => format!("{} {}", if required { "required" } else { "optional" }, f.type_spec),
        };
        match f.kind {
            Kind::Flag => {
                let d = f.default.as_ref().map(|d| d[0].as_str()).unwrap_or("false");
                out.push_str(&format!("{pad}{}: {d}{}\n", f.name, comment(&[kind, doc])));
            }
            Kind::Many | Kind::Single { .. } => {
                let sub = self.model(&f.type_spec).expect("nested models are compiled");
                if stack.contains(&sub.name) {
                    out.push_str(&format!("{pad}{}: {}{}\n", f.name, if f.kind == Kind::Many { "[]" } else { "{}" }, comment(&[kind, format!("recursive: see {} above", sub.name), doc])));
                    return;
                }
                out.push_str(&format!("{pad}{}:{}\n", f.name, comment(&[kind, doc])));
                stack.push(sub.name.clone());
                self.skeleton_fields(sub, inner, f.kind == Kind::Many, stack, out);
                stack.pop();
            }
            _ => {
                let ty = m.field_type(i).expect("value fields have types");
                let parts = ty.parts();
                if !parts.is_empty() {
                    out.push_str(&format!("{pad}{}:{}\n", f.name, comment(&[kind, doc])));
                    for (name, pty) in parts {
                        let hint = if pty.allows_empty() { "may be omitted".to_string() } else { String::new() };
                        out.push_str(&format!("{}{name}: {}{}\n", " ".repeat(inner), placeholder(pty), comment(&[hint])));
                    }
                    return;
                }
                let (value, detail) = match &f.default {
                    Some(d) => (d.join(" "), spell(&ty.describe())),
                    None if ty.describe().contains('.') || ty.elem().is_some() => (placeholder(ty), String::new()),
                    None => (placeholder(ty), ty.hint().unwrap_or_default()),
                };
                out.push_str(&format!("{pad}{}: {value}{}\n", f.name, comment(&[kind, detail, doc])));
            }
        }
    }
}
