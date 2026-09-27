//! The `.nct` file format: dialect, type, model and fragment declarations, and templates.
//!
//! ```text
//! type action = "permit" | "deny"
//! type vrf = /[A-Z0-9_-]{1,32}/
//!
//! model Interface
//!   name: key string
//!   description: phrase?
//!   mtu: int(576..9216) = 1500
//!   shutdown: flag
//!   subinterfaces: [Subinterface]
//!
//! template Interface
//!   interface {{ name }}
//!    description {{ description }}
//!    mtu {{ mtu }}
//!    shutdown [[ shutdown ]]
//!    << subinterfaces >>
//! ```
//!
//! A `template NAME` section names its model; it may live anywhere in the set. Pairing is
//! resolved when the set is built (`Engine::build`), not here.

use crate::value::Value;
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
    /// `[Model]`: a collection of blocks/groups.
    Many,
    /// `Model` / `Model?`: at most one block/group of a nested model.
    Single { required: bool },
}

impl Kind {
    /// Bound with `<< >>`: a nested model rather than tokens on a line.
    pub fn is_nested(self) -> bool {
        matches!(self, Kind::Many | Kind::Single { .. })
    }
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub kind: Kind,
    /// Scalar type spec (`ipv4`, `int(1..10)`) or the nested model name for `Many`/`Single`.
    pub type_spec: String,
    /// Default as written in the file (config tokens, or `true`/`false` for flags).
    pub default: Option<Vec<String>>,
    /// Trailing `# ...` on the declaration line.
    pub doc: Option<String>,
    /// `[Model] ordered`: a positional collection (identity is position, not key).
    pub ordered: bool,
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
    /// `"literal"`, optionally mapped to a data value: `"up" -> true`.
    Lit(String, Option<Value>),
    Type(String),
}

#[derive(Debug, Clone)]
pub struct ModelDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
    /// Template text, dedented, with original line numbers preserved via `template_line`.
    /// Filled in when the set is built and the `template NAME` section is found.
    pub template: String,
    /// Line number in the template's source file where the template text starts.
    pub template_line: usize,
    /// The file holding the template (may differ from `source`).
    pub template_source: String,
    pub source: String,
    pub line: usize,
    /// Declared with `fragment` rather than `model`: spliced into models with `<< @Name >>`.
    pub fragment: bool,
    /// The comment block directly above `model NAME` (no blank line between).
    pub doc: Option<String>,
}

/// A `template NAME` section: config-shaped text for the model (or fragment) `NAME`.
#[derive(Debug, Clone)]
pub struct TemplateDef {
    pub model: String,
    pub text: String,
    pub first_line: usize,
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
    pub templates: Vec<TemplateDef>,
    pub dialect: Option<DialectDef>,
    /// Deprecations and other non-fatal notes, each naming file and line.
    pub warnings: Vec<String>,
}

pub fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Dedent template lines, keeping blank lines so line numbers map 1:1 to the file.
/// `##` lines are template comments: they become blank lines and never reach the lexer.
fn template_text(lines: &[(usize, &str)]) -> String {
    let lines: Vec<(usize, &str)> = lines.iter().map(|&(n, l)| (n, if l.trim_start().starts_with("##") { "" } else { l })).collect();
    let min_indent = lines.iter().filter(|(_, l)| !l.trim().is_empty())
        .map(|(_, l)| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut text = String::new();
    for (_, l) in &lines {
        if !l.trim().is_empty() { text.push_str(&l[min_indent.min(l.len() - l.trim_start().len())..]); }
        text.push('\n');
    }
    text
}

enum Section<'a> {
    None,
    Dialect(DialectDef),
    Model(ModelDef),
    Template(TemplateDef, Vec<(usize, &'a str)>),
}

/// Parse one `.nct` file. `source` names it in error messages.
pub fn parse(source: &str, text: &str) -> Result<File> {
    let mut file = File::default();
    let mut errors: Vec<String> = Vec::new();
    let err = |errors: &mut Vec<String>, ln: usize, msg: String| errors.push(format!("{source}:{ln}: {msg}"));
    let mut section = Section::None;
    // The most recent model/fragment in this file, for the deprecated bare `template`.
    let mut last_model: Option<String> = None;
    // Top-level comment lines directly above the current line: a model's doc.
    let mut comments: Vec<&str> = Vec::new();

    fn close(section: &mut Section<'_>, file: &mut File, errors: &mut Vec<String>, source: &str) {
        match std::mem::replace(section, Section::None) {
            Section::None => {}
            Section::Dialect(d) => {
                if file.dialect.is_some() { errors.push(format!("{source}:{}: only one `dialect` section per file", d.line)); }
                file.dialect = Some(d);
            }
            Section::Model(m) => file.models.push(m),
            Section::Template(mut t, lines) => {
                t.text = template_text(&lines);
                t.first_line = lines.first().map(|(ln, _)| *ln).unwrap_or(t.line + 1);
                if t.text.trim().is_empty() { errors.push(format!("{source}:{}: template {} is empty", t.line, t.model)); }
                file.templates.push(t);
            }
        }
    }

    for (i, raw) in text.lines().enumerate() {
        let ln = i + 1;
        let indented = raw.starts_with(' ') || raw.starts_with('\t');
        let line = raw.trim_end();

        if let Section::Template(_, lines) = &mut section {
            if indented || line.trim().is_empty() {
                lines.push((ln, line));
                continue;
            }
        }
        let t = line.trim();
        if t.is_empty() {
            comments.clear();
            continue;
        }
        if t.starts_with('#') {
            if !indented { comments.push(t.trim_start_matches('#').trim()); }
            continue;
        }
        let doc = std::mem::take(&mut comments);
        if indented {
            match &mut section {
                Section::Dialect(d) => match t.split_once(':') {
                    Some((k, v)) => d.props.push((k.trim().to_string(), v.trim().to_string(), ln)),
                    None => err(&mut errors, ln, "expected `key: value` in the dialect section".into()),
                },
                Section::Model(m) => match parse_field(t, ln) {
                    Ok(f) => {
                        if m.fields.iter().any(|g| g.name == f.name) { err(&mut errors, ln, format!("duplicate field `{}`", f.name)); }
                        m.fields.push(f);
                    }
                    Err(e) => err(&mut errors, ln, e.0),
                },
                _ => err(&mut errors, ln, "field declaration outside a `model` or `fragment` section".into()),
            }
            continue;
        }
        close(&mut section, &mut file, &mut errors, source);
        let mut words = t.splitn(2, ' ');
        let kw = words.next().unwrap_or("");
        let rest = words.next().unwrap_or("").trim();
        match kw {
            "type" => match parse_type(rest) {
                Ok((name, def)) => file.types.push(TypeDef { name, def, line: ln }),
                Err(e) => err(&mut errors, ln, e),
            },
            "dialect" => {
                if !is_ident(rest) { err(&mut errors, ln, format!("`{rest}` is not a valid dialect name")); }
                section = Section::Dialect(DialectDef { name: rest.to_string(), props: Vec::new(), line: ln });
            }
            "model" | "fragment" => {
                if !is_ident(rest) { err(&mut errors, ln, format!("`{rest}` is not a valid {kw} name")); }
                last_model = Some(rest.to_string());
                section = Section::Model(ModelDef {
                    name: rest.to_string(), fields: Vec::new(), template: String::new(), template_line: ln,
                    template_source: source.to_string(), source: source.to_string(), line: ln, fragment: kw == "fragment",
                    doc: if doc.is_empty() { None } else { Some(doc.join(" ")) },
                });
            }
            "template" => {
                let name = if rest.is_empty() {
                    match &last_model {
                        Some(m) => {
                            file.warnings.push(format!("{source}:{ln}: bare `template` is deprecated; write `template {m}` (`netcfg fmt` rewrites files)"));
                            m.clone()
                        }
                        None => { err(&mut errors, ln, "`template` needs the name of its model: `template NAME`".into()); continue; }
                    }
                } else if is_ident(rest) {
                    rest.to_string()
                } else {
                    err(&mut errors, ln, format!("`template {rest}`: expected `template NAME` naming a model; the text goes on the indented lines below"));
                    continue;
                };
                section = Section::Template(TemplateDef { model: name, text: String::new(), first_line: ln + 1, source: source.to_string(), line: ln }, Vec::new());
            }
            other => err(&mut errors, ln, format!("unexpected `{other}`; expected `dialect`, `type`, `model`, `fragment` or `template`")),
        }
    }
    close(&mut section, &mut file, &mut errors, source);

    if errors.is_empty() { Ok(file) } else { Err(Error(errors.join("\n"))) }
}

/// `NAME = body` of a `type` line.
fn parse_type(rest: &str) -> std::result::Result<(String, TypeBody), String> {
    let (name, body) = match rest.split_once('=') {
        Some((n, b)) => (n.trim(), b.trim()),
        None => return Err("expected `type NAME = /regex/`, `type NAME = \"a\" | \"b\"` or `type NAME = {{ part: type }} ...`".into()),
    };
    if !is_ident(name) { return Err(format!("`{name}` is not a valid type name")); }
    let def = if body.contains("{{") {
        TypeBody::Struct(parse_struct_body(body).map_err(|e| format!("type `{name}`: {e}"))?)
    } else if body.starts_with('/') && body.ends_with('/') && body.len() >= 2 {
        let re = &body[1..body.len() - 1];
        regex::Regex::new(re).map_err(|e| format!("invalid regex for type `{name}`: {e}"))?;
        TypeBody::Regex(re.to_string())
    } else {
        let alts = parse_alts(body).map_err(|e| format!("type `{name}`: {e}"))?;
        if alts.is_empty() { return Err(format!("type `{name}` needs at least one alternative")); }
        TypeBody::Union(alts)
    };
    Ok((name.to_string(), def))
}

pub fn parse_struct_body(body: &str) -> std::result::Result<Vec<StructTok>, String> {
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
/// A literal may map to a data value: `"up" -> true | "down" -> false`.
pub fn parse_alts(spec: &str) -> std::result::Result<Vec<Alt>, String> {
    let mut alts = Vec::new();
    for part in spec.split('|').map(str::trim) {
        let (part, mapped) = match part.split_once("->") {
            Some((p, v)) => (p.trim(), Some(parse_mapped(v.trim())?)),
            None => (part, None),
        };
        if let Some(lit) = part.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            alts.push(Alt::Lit(lit.to_string(), mapped));
        } else if mapped.is_some() {
            return Err(format!("`{part} -> ...`: only a \"quoted\" literal can be mapped to a value"));
        } else if is_ident(part) || part.starts_with("int(") || part.starts_with("list(") {
            alts.push(Alt::Type(part.to_string()));
        } else {
            return Err(format!("`{part}` is neither a type name nor a \"quoted\" literal"));
        }
    }
    Ok(alts)
}

/// Split a trailing `# comment` off a declaration line (a `#` inside quotes is literal).
fn split_doc(t: &str) -> (&str, Option<String>) {
    let mut quoted = false;
    for (i, c) in t.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => {
                let doc = t[i..].trim_start_matches('#').trim();
                return (t[..i].trim_end(), if doc.is_empty() { None } else { Some(doc.to_string()) });
            }
            _ => {}
        }
    }
    (t, None)
}

/// The data side of `"literal" -> value`: true, false, an integer or a "quoted string".
fn parse_mapped(v: &str) -> std::result::Result<Value, String> {
    match v {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => {
            if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) { return Ok(Value::Str(s.to_string())); }
            v.parse::<i64>().map(Value::Int).map_err(|_| format!("`-> {v}`: a literal maps to true, false, an integer or a \"quoted string\""))
        }
    }
}

fn parse_field(t: &str, ln: usize) -> Result<FieldDef> {
    let (t, doc) = split_doc(t);
    let (name, spec) = t.split_once(':').ok_or_else(|| Error(format!("expected `name: type`, got `{t}`")))?;
    let name = name.trim();
    if !is_ident(name) { return Err(Error(format!("`{name}` is not a valid field name"))); }
    let (spec, default) = if spec.contains("{{") { (spec.trim(), None) } else { match spec.split_once('=') {
        Some((s, d)) => (s.trim(), Some(d.split_ascii_whitespace().map(String::from).collect::<Vec<_>>())),
        None => (spec.trim(), None),
    } };
    if let Some(d) = &default { if d.is_empty() { return Err(Error(format!("field `{name}`: empty default"))); } }
    let (spec, ordered) = match spec.strip_suffix(" ordered") {
        Some(s) if s.trim_end().ends_with(']') => (s.trim_end(), true),
        _ => (spec, false),
    };
    let (key, spec) = match spec.strip_prefix("key ") {
        Some(s) => (true, s.trim()),
        None => (false, spec),
    };
    // Anonymous struct: `{{ limit: int }} {{ action: "warning-only" | "" }}?`
    let (spec, anon_opt) = match spec.strip_suffix('?') { Some(inner) if spec.contains("{{") => (inner.trim(), true), _ => (spec, false) };
    let (kind, type_spec) = if spec.contains("{{") {
        parse_struct_body(spec).map_err(|e| Error(format!("field `{name}`: {e}")))?;
        (if anon_opt { Kind::Opt } else if key { Kind::Key } else { Kind::Scalar }, spec.to_string())
    } else if spec == "flag" {
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
    Ok(FieldDef { name: name.to_string(), kind, type_spec, default, doc, ordered, line: ln })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_model() {
        let f = parse("t.nct", "type action = \"permit\" | \"deny\"\n\nmodel Rm\n  name: key string\n  action: action\n  seq: key int\n  desc: phrase?\n  shut: flag = true\n  kids: [Kid]\n\ntemplate Rm\n  route-map {{ name }} {{ action }} {{ seq }}\n    description {{ desc }}\n").unwrap();
        assert_eq!(f.types.len(), 1);
        let m = &f.models[0];
        assert_eq!(m.fields.len(), 6);
        assert_eq!(m.fields[0].kind, Kind::Key);
        assert_eq!(m.fields[3].kind, Kind::Opt);
        assert_eq!(m.fields[4].default, Some(vec!["true".into()]));
        assert_eq!(m.fields[5].type_spec, "Kid");
        assert!(!m.fields[5].ordered);
        let g = parse("t.nct", "model Acl\n  name: key string\n  entries: [AclEntry] ordered   # in config order\n").unwrap();
        let e = &g.models[0].fields[1];
        assert_eq!((e.kind, e.type_spec.as_str(), e.ordered, e.doc.as_deref()), (Kind::Many, "AclEntry", true, Some("in config order")));
        let t = &f.templates[0];
        assert_eq!(t.model, "Rm");
        assert!(t.text.starts_with("route-map"));
        assert_eq!(t.first_line, 12);
        assert!(f.warnings.is_empty());
        let f = parse("d.nct", "dialect iosxe\n  extends: ios\n  cidr: masked\n").unwrap();
        assert_eq!(f.dialect.as_ref().unwrap().props.len(), 2);
    }

    #[test]
    fn templates_name_their_model_anywhere() {
        let f = parse("t.nct", "template B\n  b {{ x }}\n\nmodel A\n  x: int\n\nmodel B\n  x: int\n\ntemplate A\n  a {{ x }}\n").unwrap();
        assert_eq!(f.models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(f.templates.iter().map(|t| (t.model.as_str(), t.first_line)).collect::<Vec<_>>(), vec![("B", 2), ("A", 11)]);
        let f = parse("t.nct", "fragment Common\n  mtu: int?\n\ntemplate Common\n  mtu {{ mtu }}\n").unwrap();
        assert!(f.models[0].fragment);
    }

    #[test]
    fn docs_and_template_comments() {
        let f = parse("t.nct", "# unrelated\n\n# A BGP neighbor.\n# One per peer.\nmodel N\n  peer: key ip   # the peer's address\n  mode: \"#a\" | \"b\"?  # quoted # is literal\n  x: int?\n\ntemplate N\n  ## a template comment\n  neighbor {{ peer }}\n    ## indented too\n    # literal\n").unwrap();
        let m = &f.models[0];
        assert_eq!(m.doc.as_deref(), Some("A BGP neighbor. One per peer."));
        assert_eq!(m.fields[0].doc.as_deref(), Some("the peer's address"));
        assert_eq!(m.fields[0].type_spec, "ip");
        assert_eq!(m.fields[1].type_spec, "\"#a\" | \"b\"");
        assert_eq!(m.fields[1].doc.as_deref(), Some("quoted # is literal"));
        assert_eq!(m.fields[2].doc, None);
        assert_eq!(f.templates[0].text, "\nneighbor {{ peer }}\n\n  # literal\n");
    }

    #[test]
    fn bare_template_is_deprecated() {
        let f = parse("old.nct", "model A\n  x: int\n\ntemplate\n  a {{ x }}\n").unwrap();
        assert_eq!(f.templates[0].model, "A");
        assert_eq!(f.warnings, vec!["old.nct:4: bare `template` is deprecated; write `template A` (`netcfg fmt` rewrites files)"]);
        let err = parse("t.nct", "template\n  a {{ x }}\n").unwrap_err();
        assert!(err.0.contains("t.nct:1: `template` needs the name of its model"), "{err}");
        let err = parse("t.nct", "model A\n  x: int\ntemplate A B\n  a\n").unwrap_err();
        assert!(err.0.contains("t.nct:3: `template A B`"), "{err}");
    }
}
