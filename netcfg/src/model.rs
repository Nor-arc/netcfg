//! The `.ttp` file format: type declarations, model declarations, and template text.
//!
//! ```text
//! type action = permit | deny
//! type vrf = /[A-Z0-9_-]{1,32}/
//!
//! model Interface
//!   name: key string
//!   description: phrase?
//!   mtu: int(576..9216) = 1500
//!   shutdown: flag
//!   subinterfaces: [Subinterface]
//!
//! template
//!   interface {{ name }}
//!    description {{ description }}
//!    mtu {{ mtu }}
//!    shutdown {{ shutdown }}
//!    {{ subinterfaces }}
//! ```

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Part of the block's identity.
    Key,
    /// Required value (or with a default).
    Scalar,
    /// `type?`: the line may be absent.
    Opt,
    /// `flag`: presence of a literal line.
    Flag,
    /// `[Model]`: a keyed collection.
    Many,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub kind: Kind,
    /// Scalar type spec (`ipv4`, `int(1..10)`) or the element model name for `Many`.
    pub type_spec: String,
    /// Default as written in the file (config tokens, or `true`/`false` for flags).
    pub default: Option<Vec<String>>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub def: TypeBody,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub enum TypeBody {
    Regex(String),
    /// Alternatives tried in order: `"literal"` tokens or references to other types.
    Union(Vec<Alt>),
    /// A structured value: literals and named placeholders, e.g.
    /// `{{ limit: int }} {{ action: "warning-only" | "" }}`. The value is a record.
    Struct(Vec<StructTok>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructTok {
    Lit(String),
    /// `{{ name: spec }}`; `spec` is a type name/spec or an inline `a | "b"` union.
    Field { name: String, spec: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alt {
    Lit(String),
    Type(String),
}

#[derive(Debug, Clone)]
pub struct ModelDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
    /// Template text, dedented, with original line numbers preserved via `template_line`.
    pub template: String,
    /// Line number in the source file where the template text starts.
    pub template_line: usize,
    pub source: String,
    pub line: usize,
}

/// A `dialect NAME` section: properties as written, resolved by `Dialect::from_props`.
#[derive(Debug, Clone)]
pub struct DialectDef {
    pub name: String,
    pub props: Vec<(String, String, usize)>,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct File {
    pub types: Vec<TypeDef>,
    pub models: Vec<ModelDef>,
    pub dialect: Option<DialectDef>,
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse one `.ttp` file. `source` names it in error messages.
pub fn parse(source: &str, text: &str) -> Result<File> {
    let mut file = File::default();
    let mut errors: Vec<String> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let err = |errors: &mut Vec<String>, ln: usize, msg: String| errors.push(format!("{source}:{ln}: {msg}"));

    // A model under construction: fields collected, template pending.
    let mut current: Option<ModelDef> = None;
    let mut in_dialect: Option<DialectDef> = None;
    let mut in_template = false;
    let mut template_lines: Vec<(usize, &str)> = Vec::new();

    fn finish(current: &mut Option<ModelDef>, template_lines: &mut Vec<(usize, &str)>, file: &mut File, errors: &mut Vec<String>, source: &str) {
        if let Some(mut m) = current.take() {
            let min_indent = template_lines.iter().filter(|(_, l)| !l.trim().is_empty())
                .map(|(_, l)| l.len() - l.trim_start().len()).min().unwrap_or(0);
            // Keep blank lines so line numbers inside the template map 1:1 to the file.
            let mut text = String::new();
            for (_, l) in template_lines.iter() {
                if !l.trim().is_empty() { text.push_str(&l[min_indent.min(l.len() - l.trim_start().len())..]); }
                text.push('\n');
            }
            if text.trim().is_empty() {
                errors.push(format!("{source}:{}: model {} has no template", m.line, m.name));
            }
            m.template = text;
            m.template_line = template_lines.first().map(|(ln, _)| *ln).unwrap_or(m.line);
            file.models.push(m);
        }
        template_lines.clear();
    }

    while i < lines.len() {
        let ln = i + 1;
        let raw = lines[i];
        i += 1;
        let indented = raw.starts_with(' ') || raw.starts_with('\t');
        let line = raw.trim_end();

        if in_template {
            if indented || line.trim().is_empty() {
                template_lines.push((ln, line));
                continue;
            }
            in_template = false;
            finish(&mut current, &mut template_lines, &mut file, &mut errors, source);
        }

        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if indented {
            if let Some(d) = in_dialect.as_mut() {
                match t.split_once(':') {
                    Some((k, v)) => d.props.push((k.trim().to_string(), v.trim().to_string(), ln)),
                    None => err(&mut errors, ln, "expected `key: value` in the dialect section".into()),
                }
                continue;
            }
        } else if let Some(d) = in_dialect.take() {
            if file.dialect.is_some() { err(&mut errors, d.line, "only one `dialect` section per file".into()); }
            file.dialect = Some(d);
        }
        if !indented {
            let mut words = t.splitn(2, ' ');
            let kw = words.next().unwrap_or("");
            let rest = words.next().unwrap_or("").trim();
            match kw {
                "type" => {
                    let (name, body) = match rest.split_once('=') {
                        Some((n, b)) => (n.trim(), b.trim()),
                        None => { err(&mut errors, ln, "expected `type NAME = /regex/` or `type NAME = a | b`".into()); continue; }
                    };
                    if !is_ident(name) { err(&mut errors, ln, format!("`{name}` is not a valid type name")); continue; }
                    let def = if body.contains("{{") {
                        match parse_struct_body(body) {
                            Ok(toks) => TypeBody::Struct(toks),
                            Err(e) => { err(&mut errors, ln, format!("type `{name}`: {e}")); continue; }
                        }
                    } else if body.starts_with('/') && body.ends_with('/') && body.len() >= 2 {
                        match regex::Regex::new(&body[1..body.len() - 1]) {
                            Ok(_) => TypeBody::Regex(body[1..body.len() - 1].to_string()),
                            Err(e) => { err(&mut errors, ln, format!("invalid regex for type `{name}`: {e}")); continue; }
                        }
                    } else {
                        let mut alts = Vec::new();
                        for part in body.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                            if let Some(lit) = part.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                                alts.push(Alt::Lit(lit.to_string()));
                            } else if is_ident(part) || part.starts_with("int(") || part.starts_with("list(") {
                                alts.push(Alt::Type(part.to_string()));
                            } else {
                                err(&mut errors, ln, format!("type `{name}`: `{part}` is neither a type name nor a \"quoted\" literal"));
                            }
                        }
                        if alts.is_empty() { err(&mut errors, ln, format!("type `{name}` needs at least one alternative")); continue; }
                        TypeBody::Union(alts)
                    };
                    file.types.push(TypeDef { name: name.to_string(), def, line: ln });
                }
                "dialect" => {
                    finish(&mut current, &mut template_lines, &mut file, &mut errors, source);
                    if !is_ident(rest) { err(&mut errors, ln, format!("`{rest}` is not a valid dialect name")); }
                    in_dialect = Some(DialectDef { name: rest.to_string(), props: Vec::new(), line: ln });
                }
                "model" => {
                    finish(&mut current, &mut template_lines, &mut file, &mut errors, source);
                    if !is_ident(rest) { err(&mut errors, ln, format!("`{rest}` is not a valid model name")); }
                    current = Some(ModelDef { name: rest.to_string(), fields: Vec::new(), template: String::new(), template_line: ln, source: source.to_string(), line: ln });
                }
                "template" => {
                    if current.is_none() { err(&mut errors, ln, "`template` must follow a `model` section".into()); continue; }
                    if !rest.is_empty() { err(&mut errors, ln, "`template` takes no arguments; the text goes on the indented lines below".into()); }
                    in_template = true;
                }
                other => err(&mut errors, ln, format!("unexpected `{other}`; expected `dialect`, `type`, `model` or `template`")),
            }
            continue;
        }

        // Indented: a field of the current model.
        let Some(m) = current.as_mut() else {
            err(&mut errors, ln, "field declaration outside a `model` section".into());
            continue;
        };
        match parse_field(t, ln) {
            Ok(f) => {
                if m.fields.iter().any(|g| g.name == f.name) { err(&mut errors, ln, format!("duplicate field `{}`", f.name)); }
                m.fields.push(f);
            }
            Err(e) => err(&mut errors, ln, e.0),
        }
    }
    finish(&mut current, &mut template_lines, &mut file, &mut errors, source);
    if let Some(d) = in_dialect.take() {
        if file.dialect.is_some() { err(&mut errors, d.line, "only one `dialect` section per file".into()); }
        file.dialect = Some(d);
    }

    if errors.is_empty() { Ok(file) } else { Err(Error(errors.join("\n"))) }
}

fn parse_struct_body(body: &str) -> std::result::Result<Vec<StructTok>, String> {
    let mut toks = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        match rest.find("{{") {
            Some(start) => {
                for w in rest[..start].split_ascii_whitespace() { toks.push(StructTok::Lit(w.to_string())); }
                let end = rest[start..].find("}}").ok_or("unterminated `{{`")? + start;
                let inner = rest[start + 2..end].trim();
                let (name, spec) = inner.split_once(':').ok_or_else(|| format!("placeholder `{{{{ {inner} }}}}` must be `{{{{ name: type }}}}`"))?;
                let (name, spec) = (name.trim(), spec.trim());
                if !is_ident(name) { return Err(format!("`{name}` is not a valid field name")); }
                if spec.is_empty() { return Err(format!("placeholder `{name}` has no type")); }
                if toks.iter().any(|t| matches!(t, StructTok::Field { name: n, .. } if n == name)) { return Err(format!("duplicate field `{name}`")); }
                toks.push(StructTok::Field { name: name.to_string(), spec: spec.to_string() });
                rest = &rest[end + 2..];
            }
            None => {
                for w in rest.split_ascii_whitespace() { toks.push(StructTok::Lit(w.to_string())); }
                rest = "";
            }
        }
    }
    if !toks.iter().any(|t| matches!(t, StructTok::Field { .. })) { return Err("a struct type needs at least one placeholder".into()); }
    Ok(toks)
}

/// Split an inline union spec (`asn | "auto" | ""`) into alternatives; a plain spec yields one type alt.
pub fn parse_alts(spec: &str) -> std::result::Result<Vec<Alt>, String> {
    let mut alts = Vec::new();
    for part in spec.split('|').map(str::trim) {
        if let Some(lit) = part.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            alts.push(Alt::Lit(lit.to_string()));
        } else if is_ident(part) || part.starts_with("int(") || part.starts_with("list(") {
            alts.push(Alt::Type(part.to_string()));
        } else {
            return Err(format!("`{part}` is neither a type name nor a \"quoted\" literal"));
        }
    }
    Ok(alts)
}

fn parse_field(t: &str, ln: usize) -> Result<FieldDef> {
    let (name, spec) = t.split_once(':').ok_or_else(|| Error(format!("expected `name: type`, got `{t}`")))?;
    let name = name.trim();
    if !is_ident(name) { return Err(Error(format!("`{name}` is not a valid field name"))); }
    let (spec, default) = match spec.split_once('=') {
        Some((s, d)) => (s.trim(), Some(d.split_ascii_whitespace().map(String::from).collect::<Vec<_>>())),
        None => (spec.trim(), None),
    };
    if let Some(d) = &default { if d.is_empty() { return Err(Error(format!("field `{name}`: empty default"))); } }
    let (key, spec) = match spec.strip_prefix("key ") {
        Some(s) => (true, s.trim()),
        None => (false, spec),
    };
    let (kind, type_spec) = if spec == "flag" {
        (Kind::Flag, "flag".to_string())
    } else if let Some(inner) = spec.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if !is_ident(inner.trim()) { return Err(Error(format!("field `{name}`: `{inner}` is not a model name"))); }
        (Kind::Many, inner.trim().to_string())
    } else if let Some(inner) = spec.strip_suffix('?') {
        (Kind::Opt, inner.trim().to_string())
    } else {
        (if key { Kind::Key } else { Kind::Scalar }, spec.to_string())
    };
    if key && kind != Kind::Key { return Err(Error(format!("field `{name}`: a key must be a plain scalar type"))); }
    if kind == Kind::Flag {
        if let Some(d) = &default {
            if !(d == &["true".to_string()] || d == &["false".to_string()]) { return Err(Error(format!("field `{name}`: a flag default must be true or false"))); }
        }
    }
    if default.is_some() && matches!(kind, Kind::Opt | Kind::Many | Kind::Key) {
        return Err(Error(format!("field `{name}`: only required values and flags can have a default")));
    }
    if type_spec.is_empty() { return Err(Error(format!("field `{name}`: missing type"))); }
    Ok(FieldDef { name: name.to_string(), kind, type_spec, default, line: ln })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_model() {
        let f = parse("t.ttp", "type action = \"permit\" | \"deny\"\n\nmodel Rm\n  name: key string\n  action: action\n  seq: key int\n  desc: phrase?\n  shut: flag = true\n  kids: [Kid]\n\ntemplate\n  route-map {{ name }} {{ action }} {{ seq }}\n    description {{ desc }}\n").unwrap();
        assert_eq!(f.types.len(), 1);
        let m = &f.models[0];
        assert_eq!(m.fields.len(), 6);
        assert_eq!(m.fields[0].kind, Kind::Key);
        assert_eq!(m.fields[3].kind, Kind::Opt);
        assert_eq!(m.fields[4].default, Some(vec!["true".into()]));
        assert_eq!(m.fields[5].type_spec, "Kid");
        assert!(m.template.starts_with("route-map"));
        assert_eq!(m.template_line, 12);
        let f = parse("d.ttp", "dialect iosxe\n  extends: ios\n  cidr: masked\n").unwrap();
        assert_eq!(f.dialect.as_ref().unwrap().props.len(), 2);
    }
}
