//! Dialects are declared, not coded. A `dialect` section (in `dialect.ttp` or any template
//! file) names the grammar and the rendering and type conventions:
//!
//! ```text
//! dialect nxos
//!   extends: cisco
//!   grammar: indent          # indent | braces
//!   indent: 2
//!   comments: !
//!   skip: end, Building configuration, Current configuration
//!   block-separator: !       # written after each top-level block (IOS)
//!   end-marker: end          # written at the end (IOS)
//!   negation: no             # prefix that negates a flag line
//!   delete: no               # prefix that removes a statement/block (defaults to negation)
//!   cidr: slash              # a type convention: slash | masked
//!   render: structured       # structured | set  (braces grammar only)
//! ```
//! Only new *grammars* (how text becomes a tree) and new *type implementations* are code.

use crate::lexer::{lex_braces, lex_indent, Node, OwnedNode};
use crate::{Error, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grammar {
    Indent,
    Braces,
}

#[derive(Debug, Clone)]
pub struct Dialect {
    pub name: String,
    pub grammar: Grammar,
    pub indent: usize,
    /// Lines starting with any of these are comments (indent grammar).
    pub comments: Vec<String>,
    /// Lines starting with any of these are dropped (banners, `end`).
    pub skip: Vec<String>,
    pub block_separator: Option<String>,
    pub end_marker: Option<String>,
    /// Prefix that negates a flag line (`no shutdown`).
    pub negation: Option<String>,
    /// Prefix that removes a statement or block in a change set: `no` (indent dialects, the
    /// default is the negation word), `delete` (Junos `set` style: `delete protocols bgp`).
    pub delete: Option<String>,
    /// Braces grammar: render as `set` commands instead of structured text.
    pub render_set: bool,
    /// Type conventions, e.g. `cidr: masked`.
    pub knobs: HashMap<String, String>,
}

/// Builtin declarations, in the same format users write.
pub const BUILTINS: &[(&str, &str)] = &[
    ("cisco", "dialect cisco\n  grammar: indent\n  indent: 1\n  comments: !\n  skip: end, Building configuration, Current configuration\n  negation: no\n  cidr: slash\n"),
    ("ios", "dialect ios\n  extends: cisco\n  indent: 1\n  block-separator: !\n  end-marker: end\n  cidr: masked\n"),
    ("nxos", "dialect nxos\n  extends: cisco\n  indent: 2\n"),
    ("eos", "dialect eos\n  extends: cisco\n  indent: 3\n"),
    ("junos", "dialect junos\n  grammar: braces\n  indent: 4\n  cidr: slash\n  render: structured\n  delete: delete\n"),
];

impl Dialect {
    pub fn builtin(name: &str) -> Option<Dialect> {
        let text = BUILTINS.iter().find(|(n, _)| *n == name)?.1;
        Dialect::from_text(text).ok()
    }

    pub fn builtin_names() -> Vec<&'static str> {
        BUILTINS.iter().map(|(n, _)| *n).collect()
    }

    /// Parse a declaration; `extends:` may name a builtin.
    pub fn from_text(text: &str) -> Result<Dialect> {
        let mut name: Option<String> = None;
        let mut props: Vec<(String, String, usize)> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let t = raw.trim();
            if t.is_empty() || t.starts_with('#') { continue; }
            if !raw.starts_with(' ') && !raw.starts_with('\t') {
                match t.strip_prefix("dialect ") {
                    Some(n) if name.is_none() => name = Some(n.trim().to_string()),
                    _ => return Err(Error(format!("line {}: expected `dialect NAME`", i + 1))),
                }
            } else {
                let (k, v) = t.split_once(':').ok_or_else(|| Error(format!("line {}: expected `key: value`", i + 1)))?;
                props.push((k.trim().to_string(), v.trim().to_string(), i + 1));
            }
        }
        let name = name.ok_or_else(|| Error("missing `dialect NAME` line".into()))?;
        Dialect::from_props(&name, &props)
    }

    pub fn from_props(name: &str, props: &[(String, String, usize)]) -> Result<Dialect> {
        let mut d = match props.iter().find(|(k, _, _)| k == "extends") {
            Some((_, base, ln)) => Dialect::builtin(base).ok_or_else(|| Error(format!("line {ln}: unknown base dialect `{base}` (builtin: {})", Dialect::builtin_names().join(", "))))?,
            None => Dialect { name: String::new(), grammar: Grammar::Indent, indent: 1, comments: Vec::new(), skip: Vec::new(), block_separator: None, end_marker: None, negation: None, delete: None, render_set: false, knobs: HashMap::new() },
        };
        d.name = name.to_string();
        let list = |v: &str| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
        let opt = |v: &str| if v == "none" || v.is_empty() { None } else { Some(v.to_string()) };
        for (k, v, ln) in props {
            match k.as_str() {
                "extends" => {}
                "grammar" => d.grammar = match v.as_str() { "indent" => Grammar::Indent, "braces" => Grammar::Braces, other => return Err(Error(format!("line {ln}: unknown grammar `{other}` (indent, braces)"))) },
                "indent" => d.indent = v.parse().map_err(|_| Error(format!("line {ln}: indent must be a number")))?,
                "comments" => d.comments = list(v),
                "skip" => d.skip = list(v),
                "block-separator" => d.block_separator = opt(v),
                "end-marker" => d.end_marker = opt(v),
                "negation" => d.negation = opt(v),
                "delete" => d.delete = opt(v),
                "render" => d.render_set = match v.as_str() { "structured" => false, "set" => true, other => return Err(Error(format!("line {ln}: render must be structured or set, got `{other}`"))) },
                other => { d.knobs.insert(other.to_string(), v.clone()); }
            }
        }
        Ok(d)
    }

    /// The word that removes a statement: `delete`, else the negation word.
    pub fn delete_word(&self) -> Option<&str> {
        self.delete.as_deref().or(self.negation.as_deref())
    }

    pub fn knob(&self, name: &str) -> Option<&str> {
        self.knobs.get(name).map(String::as_str)
    }

    pub fn lex<'a>(&self, text: &'a str) -> Vec<Node<'a>> {
        match self.grammar {
            Grammar::Indent => {
                let skip = |line: &str| self.comments.iter().any(|c| line.starts_with(c.as_str())) || self.skip.iter().any(|s| line.starts_with(s.as_str()));
                lex_indent(text, &skip)
            }
            Grammar::Braces => lex_braces(text),
        }
    }

    /// Lex template text: same grammar, but nothing is skipped, so a stray `!` line in an
    /// IOS template is reported rather than silently dropped.
    pub fn lex_template<'a>(&self, text: &'a str) -> Vec<Node<'a>> {
        match self.grammar {
            Grammar::Indent => lex_indent(text, &|_| false),
            Grammar::Braces => lex_braces(text),
        }
    }

    pub fn render(&self, nodes: &[OwnedNode]) -> String {
        let mut out = String::new();
        match self.grammar {
            Grammar::Indent => {
                fn go(d: &Dialect, n: &OwnedNode, depth: usize, out: &mut String) {
                    for _ in 0..depth * d.indent { out.push(' '); }
                    out.push_str(&n.line());
                    out.push('\n');
                    for c in &n.children { go(d, c, depth + 1, out); }
                }
                for n in nodes {
                    go(self, n, 0, &mut out);
                    if let (Some(sep), true) = (&self.block_separator, n.block || !n.children.is_empty()) { out.push_str(sep); out.push('\n'); }
                }
                if let Some(end) = &self.end_marker { out.push_str(end); out.push('\n'); }
            }
            Grammar::Braces if self.render_set => {
                fn go(n: &OwnedNode, prefix: &mut Vec<String>, out: &mut String) {
                    prefix.extend(n.tokens.iter().map(|t| quote(t)));
                    if n.children.is_empty() {
                        out.push_str("set ");
                        out.push_str(&prefix.join(" "));
                        out.push('\n');
                    } else {
                        for c in &n.children { go(c, prefix, out); }
                    }
                    prefix.truncate(prefix.len() - n.tokens.len());
                }
                for n in nodes { go(n, &mut Vec::new(), &mut out); }
            }
            Grammar::Braces => {
                fn go(d: &Dialect, n: &OwnedNode, depth: usize, out: &mut String) {
                    for _ in 0..depth * d.indent { out.push(' '); }
                    let toks: Vec<String> = n.tokens.iter().map(|t| quote(t)).collect();
                    out.push_str(&toks.join(" "));
                    if n.children.is_empty() {
                        out.push_str(";\n");
                    } else {
                        out.push_str(" {\n");
                        for c in &n.children { go(d, c, depth + 1, out); }
                        for _ in 0..depth * d.indent { out.push(' '); }
                        out.push_str("}\n");
                    }
                }
                for n in nodes { go(self, n, 0, &mut out); }
            }
        }
        out
    }
}

/// Tokens containing whitespace came from quoted strings; write them quoted again.
pub(crate) fn quote(t: &str) -> String {
    if t.contains(char::is_whitespace) || t.is_empty() { format!("\"{t}\"") } else { t.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_extend() {
        let ios = Dialect::builtin("ios").unwrap();
        assert_eq!(ios.grammar, Grammar::Indent);
        assert_eq!(ios.knob("cidr"), Some("masked"));
        assert_eq!(ios.negation.as_deref(), Some("no"));
        assert_eq!(ios.end_marker.as_deref(), Some("end"));
        let nx = Dialect::builtin("nxos").unwrap();
        assert_eq!(nx.knob("cidr"), Some("slash"));
        assert_eq!(nx.indent, 2);
        assert!(nx.end_marker.is_none());
        assert_eq!(Dialect::builtin("junos").unwrap().grammar, Grammar::Braces);
        assert_eq!(nx.delete_word(), Some("no"));
        assert_eq!(Dialect::builtin("junos").unwrap().delete_word(), Some("delete"));
        assert_eq!(Dialect::from_text("dialect x\n  extends: ios\n  delete: default\n").unwrap().delete_word(), Some("default"));
    }

    #[test]
    fn user_dialect() {
        let d = Dialect::from_text("dialect iosxe\n  extends: ios\n  indent: 1\n  skip: end, Building configuration, Current configuration, Load for\n").unwrap();
        assert_eq!(d.name, "iosxe");
        assert_eq!(d.skip.len(), 4);
        assert_eq!(d.knob("cidr"), Some("masked"));
    }

    #[test]
    fn braces_render_both_styles() {
        let n = vec![OwnedNode::with_children(vec!["system".into()], vec![OwnedNode::leaf(vec!["host-name".into(), "r1".into()]), OwnedNode::leaf(vec!["description".into(), "to core".into()])])];
        let mut d = Dialect::builtin("junos").unwrap();
        assert_eq!(d.render(&n), "system {\n    host-name r1;\n    description \"to core\";\n}\n");
        d.render_set = true;
        assert_eq!(d.render(&n), "set system host-name r1\nset system description \"to core\"\n");
    }
}
