//! Template text: config-shaped lines with `{{ value }}`, `[[ flag ]]` and `<< model >>`
//! placeholders, and `@ignore` lines.
//! Parsing and validation are pure functions over the model's field declarations, so the
//! loader, the CLI validator and a language server all agree on what a valid template is.

use crate::lexer::lex_indent as lex;
use crate::model::{FieldDef, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Lit(String),
    /// `{{ field }}`: consumes tokens.
    Hole(String),
    /// `[[ field ]]`: consumes nothing; the line's presence is the flag's value.
    Flag(String),
    /// `<< field >>`: whole statements/blocks of a nested model at this level.
    Nest(String),
}

#[derive(Debug, Clone)]
pub struct TLine {
    pub toks: Vec<Tok>,
    /// 1-based line number in the source file.
    pub line: usize,
    pub children: Vec<TLine>,
    pub ignore: bool,
}

impl TLine {
    /// Every bound field name, `{{ }}`, `[[ ]]` and `<< >>` alike.
    pub fn holes(&self) -> Vec<&str> {
        self.toks.iter().filter_map(|t| match t { Tok::Hole(n) | Tok::Flag(n) | Tok::Nest(n) => Some(n.as_str()), _ => None }).collect()
    }
    pub fn lits(&self) -> Vec<String> {
        self.toks.iter().filter_map(|t| match t { Tok::Lit(s) => Some(s.clone()), _ => None }).collect()
    }
    pub fn text(&self) -> String {
        self.toks.iter().map(|t| match t { Tok::Lit(s) => s.clone(), Tok::Hole(n) => format!("{{{{ {n} }}}}"), Tok::Flag(n) => format!("[[ {n} ]]"), Tok::Nest(n) => format!("<< {n} >>") }).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone)]
pub enum Shape {
    /// One header line with a nested body.
    Block { header: TLine, body: Vec<TLine>, ignores: Vec<Vec<String>> },
    /// Several sibling lines that all carry the key placeholders.
    Flat { lines: Vec<TLine>, ignores: Vec<Vec<String>> },
    /// No key: a document body.
    Root { body: Vec<TLine>, ignores: Vec<Vec<String>> },
}

/// Turn lexed template nodes into template lines. `first_line` is the source line number
/// of the template text's first line (node lines are relative to the text).
pub fn from_nodes(nodes: &[crate::lexer::Node<'_>], first_line: usize) -> Result<Vec<TLine>, Vec<String>> {
    let mut errors = Vec::new();
    fn conv(n: &crate::lexer::Node<'_>, first_line: usize, errors: &mut Vec<String>) -> TLine {
        let line = first_line + n.line.saturating_sub(1);
        if n.tokens.first() == Some(&"@ignore") {
            if n.tokens.len() == 1 { errors.push(format!("template line {line}: `@ignore` needs at least one word")); }
            if !n.children.is_empty() { errors.push(format!("template line {line}: `@ignore` lines cannot have children")); }
            return TLine { toks: n.tokens[1..].iter().map(|s| Tok::Lit(s.to_string())).collect(), line, children: Vec::new(), ignore: true };
        }
        let toks = n.tokens.iter().map(|w| {
            if let Some(inner) = w.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                Tok::Hole(inner.trim().to_string())
            } else if let Some(inner) = w.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")) {
                Tok::Flag(inner.trim().to_string())
            } else if let Some(inner) = w.strip_prefix("<<").and_then(|s| s.strip_suffix(">>")) {
                Tok::Nest(inner.trim().to_string())
            } else {
                if ["{{", "}}", "[[", "]]", "<<", ">>"].iter().any(|m| w.contains(m)) { errors.push(format!("template line {line}: placeholder in `{w}` must be written {{{{ name }}}} (value), [[ name ]] (flag) or << name >> (nested model)")); }
                Tok::Lit(w.to_string())
            }
        }).collect();
        let children = n.children.iter().map(|c| conv(c, first_line, errors)).collect();
        TLine { toks, line, children, ignore: false }
    }
    let lines: Vec<TLine> = nodes.iter().map(|n| conv(n, first_line, &mut errors)).collect();
    if errors.is_empty() { Ok(lines) } else { Err(errors) }
}

/// Parse template text with the indentation grammar (convenience for tests).
pub fn parse(text: &str, first_line: usize) -> Result<Vec<TLine>, Vec<String>> {
    from_nodes(&lex(text, &|_| false), first_line)
}

pub fn shape(lines: &[TLine], keys: &[&str]) -> Result<Shape, String> {
    let content: Vec<TLine> = lines.iter().filter(|l| !l.ignore).cloned().collect();
    let top_ignores: Vec<Vec<String>> = lines.iter().filter(|l| l.ignore).map(|l| l.lits()).collect();
    if keys.is_empty() {
        return Ok(Shape::Root { body: content, ignores: top_ignores });
    }
    match content.len() {
        0 => Err("template is empty".into()),
        1 => {
            if !top_ignores.is_empty() {
                return Err("`@ignore` lines of a block template go inside the block, under its header".into());
            }
            let h = &content[0];
            let body: Vec<TLine> = h.children.iter().filter(|c| !c.ignore).cloned().collect();
            let ignores = h.children.iter().filter(|c| c.ignore).map(|c| c.lits()).collect();
            let mut header = h.clone();
            header.children.clear();
            Ok(Shape::Block { header, body, ignores })
        }
        _ => Ok(Shape::Flat { lines: content, ignores: top_ignores }),
    }
}

/// `rest_of_line(type_spec)` tells whether a field's type consumes the rest of the line.
/// A line that spells the *absence* of an optional field: `<negation> <literals> {{ field }}`
/// where the same field is also bound by a normal value line. Its placeholder is the
/// only one and is the last token; the literals after the negation word must match the
/// value line's literal prefix.
pub fn absent_spelling_of<'a>(l: &'a TLine, negation: Option<&str>, fields: &[FieldDef]) -> Option<&'a str> {
    let neg = negation?;
    match l.toks.as_slice() {
        [Tok::Lit(first), .., Tok::Hole(h)] if first == neg && l.holes().len() == 1 && l.children.is_empty() => {
            fields.iter().find(|f| &f.name == h && f.kind == Kind::Opt).map(|_| h.as_str())
        }
        _ => None,
    }
}

/// A flag's negated spelling written out explicitly (`no shutdown [[ shutdown ]]` next to
/// `shutdown [[ shutdown ]]`). It is implied by the dialect anyway; writing it is allowed so
/// the template can show both spellings.
pub fn negated_flag_line<'a>(l: &'a TLine, negation: Option<&str>, fields: &[FieldDef]) -> Option<&'a str> {
    let neg = negation?;
    match l.toks.as_slice() {
        [Tok::Lit(first), .., Tok::Flag(h)] if first == neg && l.holes().len() == 1 && l.children.is_empty() && l.toks.len() > 2 => {
            fields.iter().find(|f| &f.name == h && f.kind == Kind::Flag).map(|_| h.as_str())
        }
        _ => None,
    }
}

pub fn validate(type_name: &str, lines: &[TLine], fields: &[FieldDef], rest_of_line: &dyn Fn(&FieldDef) -> bool, negation: Option<&str>) -> Vec<String> {
    let mut errs: Vec<String> = Vec::new();
    let by_name = |n: &str| fields.iter().find(|f| f.name == n);
    let keys: Vec<&str> = fields.iter().filter(|f| f.kind == Kind::Key).map(|f| f.name.as_str()).collect();
    fn all<'a>(ls: &'a [TLine], out: &mut Vec<&'a TLine>) { for l in ls { out.push(l); all(&l.children, out); } }
    let mut every: Vec<&TLine> = Vec::new();
    all(lines, &mut every);
    let err = |errs: &mut Vec<String>, l: &TLine, msg: String| errs.push(format!("template line {} `{}`: {msg}", l.line, l.text()));

    for l in &every {
        for h in l.holes() {
            if by_name(h).is_none() {
                let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                err(&mut errs, l, format!("`{h}` is not a field of {type_name} (fields: {})", names.join(", ")));
            }
        }
    }
    // Absent-spelling lines and explicit negated flag lines are checked separately and don't count as bindings.
    let absent_lines: Vec<&TLine> = every.iter().copied().filter(|l| absent_spelling_of(l, negation, fields).is_some() || negated_flag_line(l, negation, fields).is_some()).collect();
    let bound: Vec<&str> = every.iter().filter(|l| !absent_lines.iter().any(|a| std::ptr::eq(*a, **l))).flat_map(|l| l.holes()).collect();
    for f in fields {
        if !bound.contains(&f.name.as_str()) && f.default.is_none() {
            errs.push(format!("field `{}` is not bound in the template and has no default value", f.name));
        }
    }
    for a in &absent_lines {
        if let Some(h) = absent_spelling_of(a, negation, fields) {
            err(&mut errs, a, format!("the negated form of a value line is implied by the dialect's negation word: parsing `{} …` sets `{h}` to null, and null in the data renders it; remove this line", negation.unwrap_or("no")));
            continue;
        }
        let h = negated_flag_line(a, negation, fields).unwrap();
        let value_line = every.iter().find(|l| !std::ptr::eq(**l, *a) && l.holes() == vec![h] && !absent_lines.iter().any(|x| std::ptr::eq(*x, **l)));
        let what = if negated_flag_line(a, negation, fields).is_some() { "negated flag spelling" } else { "absent spelling" };
        match value_line {
            None => err(&mut errs, a, format!("`{h}` needs a normal line for this {what} to pair with")),
            Some(v) => {
                let a_lits: Vec<&Tok> = a.toks[1..a.toks.len() - 1].iter().collect();
                let v_lits: Vec<&Tok> = v.toks.iter().take_while(|t| matches!(t, Tok::Lit(_))).collect();
                if a_lits != v_lits { err(&mut errs, a, format!("{what} must be `{} <the other line's literals> {{{{ {h} }}}}`", negation.unwrap_or("no"))); }
            }
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for h in &bound {
        if keys.contains(h) { continue; }
        if seen.contains(h) {
            if !errs.iter().any(|e| e.contains(&format!("field `{h}` is bound more than once"))) {
                errs.push(format!("field `{h}` is bound more than once"));
            }
        } else { seen.push(h); }
    }

    /// A body line: either a value line (placeholders, no children) or a literal-only
    /// container whose children are themselves body lines (`protocols {`, `bgp {`).
    fn check_body_line(errs: &mut Vec<String>, l: &TLine, allow_keys: bool, allow_containers: bool, check_value_line: &dyn Fn(&mut Vec<String>, &TLine, bool)) {
        if !l.children.is_empty() && l.holes().is_empty() {
            if !allow_containers {
                errs.push(format!("template line {} `{}`: a flat group line cannot have nested lines", l.line, l.text()));
                return;
            }
            for c in l.children.iter().filter(|c| !c.ignore) { check_body_line(errs, c, allow_keys, true, check_value_line); }
            return;
        }
        check_value_line(errs, l, allow_keys);
    }
    let check_value_line = |errs: &mut Vec<String>, l: &TLine, allow_keys: bool| {
        let metas: Vec<&FieldDef> = l.holes().into_iter().filter_map(by_name).collect();
        if l.holes().is_empty() { err(errs, l, "line binds no field".into()); }
        if !l.children.is_empty() { err(errs, l, "a line with placeholders cannot have nested lines; model the block as its own keyed type".into()); }
        if !allow_keys {
            for k in l.holes() { if keys.contains(&k) { err(errs, l, format!("Key field `{k}` belongs on the header line")); } }
        }
        for t in &l.toks {
            let kind = |n: &str| by_name(n).map(|f| f.kind);
            match t {
                Tok::Hole(n) => match kind(n) {
                    Some(Kind::Flag) => err(errs, l, format!("`{n}` is a flag: write it as [[ {n} ]] (the line's presence), not {{{{ {n} }}}} (a value)")),
                    Some(k) if k.is_nested() => err(errs, l, format!("`{n}` is a nested model: write it as << {n} >> alone on its line, not {{{{ {n} }}}} (a value)")),
                    _ => {}
                },
                Tok::Flag(n) => match kind(n) {
                    Some(k) if k.is_nested() => err(errs, l, format!("`{n}` is a nested model: write it as << {n} >> alone on its line, not [[ {n} ]] (a flag)")),
                    Some(k) if k != Kind::Flag => err(errs, l, format!("`{n}` is not a flag: [[ ]] is for flags; a value is written {{{{ {n} }}}}")),
                    _ => {}
                },
                Tok::Nest(n) => match kind(n) {
                    Some(Kind::Flag) => err(errs, l, format!("`{n}` is a flag, not a nested model: write it as [[ {n} ]]")),
                    Some(k) if !k.is_nested() => err(errs, l, format!("`{n}` is a value, not a nested model: << >> is for [Model] and Model? fields; a value is written {{{{ {n} }}}}")),
                    _ => if l.toks.len() != 1 { err(errs, l, format!("<< {n} >> must be alone on its line")); },
                },
                Tok::Lit(_) => {}
            }
        }
        if let Some(m) = metas.iter().find(|m| m.kind == Kind::Flag) {
            let value_holes = l.holes().into_iter().filter(|h| by_name(h).map(|f| f.kind != Kind::Key).unwrap_or(true)).count();
            if value_holes != 1 { err(errs, l, format!("flag `{}` must be the only value placeholder on its line", m.name)); }
            if l.toks.last() != Some(&Tok::Flag(m.name.clone())) { err(errs, l, format!("flag `{}` must be the last token", m.name)); }
        }
        for m in metas.iter().filter(|m| rest_of_line(m)) {
            if l.toks.last() != Some(&Tok::Hole(m.name.clone())) { err(errs, l, format!("`{}` consumes the rest of the line, so it must be last", m.name)); }
        }
        let values: Vec<&&FieldDef> = metas.iter().filter(|m| m.kind != Kind::Key).collect();
        if values.len() > 1 && values.iter().any(|m| m.kind != Kind::Scalar || m.default.is_some()) {
            err(errs, l, "a line with several value placeholders may only bind required fields (no optional, flag, default or collection)".into());
        }
    };

    match shape(lines, &keys) {
        Err(e) => errs.push(e),
        Ok(Shape::Root { body, .. }) => { for l in &body { check_body_line(&mut errs, l, true, true, &check_value_line); } }
        Ok(Shape::Block { header, body, .. }) => {
            for k in &keys { if !header.holes().contains(k) { err(&mut errs, &header, format!("Key field `{k}` must appear on the header line")); } }
            for h in header.holes() {
                if let Some(m) = by_name(h) {
                    if m.kind != Kind::Key && m.kind != Kind::Scalar { err(&mut errs, &header, format!("`{h}` cannot be on the header line: only Key fields and required values may")); }
                    if rest_of_line(m) && header.toks.last() != Some(&Tok::Hole(m.name.clone())) { err(&mut errs, &header, format!("`{h}` consumes the rest of the line, so it must be last")); }
                }
            }
            for l in &body { check_body_line(&mut errs, l, false, true, &check_value_line); }
        }
        Ok(Shape::Flat { lines: flat, .. }) => {
            for l in &flat {
                if absent_spelling_of(l, negation, fields).is_some() || negated_flag_line(l, negation, fields).is_some() { err(&mut errs, l, "negated spellings are not supported in flat groups yet".into()); continue; }
                let ks: Vec<&str> = l.holes().into_iter().filter(|h| keys.contains(h)).collect();
                if ks != keys { err(&mut errs, l, format!("every line of a flat group must carry all Key fields in the same order ({})", keys.join(", "))); }
                let first_value = l.toks.iter().position(|t| matches!(t, Tok::Hole(n) | Tok::Flag(n) if by_name(n).map(|f| f.kind != Kind::Key).unwrap_or(false)));
                let last_key = l.toks.iter().rposition(|t| matches!(t, Tok::Hole(n) if keys.contains(&n.as_str())));
                if let (Some(fv), Some(lk)) = (first_value, last_key) { if lk > fv { err(&mut errs, l, "Key placeholders must come before value placeholders".into()); } }
                if l.holes().into_iter().filter_map(by_name).any(|m| m.kind.is_nested()) { err(&mut errs, l, "a flat group line cannot hold a nested model".into()); }
                check_body_line(&mut errs, l, true, false, &check_value_line);
            }
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_holes_and_ignores() {
        let ls = parse("interface {{ name }}\n  description {{ desc }}\n  @ignore ip address * * secondary\n  shutdown [[ shut ]]\n", 10).unwrap();
        assert_eq!(ls[0].children[2].toks, vec![Tok::Lit("shutdown".into()), Tok::Flag("shut".into())]);
        assert_eq!(ls[0].toks, vec![Tok::Lit("interface".into()), Tok::Hole("name".into())]);
        assert_eq!(ls[0].line, 10);
        assert_eq!(ls[0].children[1].ignore, true);
        assert_eq!(ls[0].children[1].lits(), vec!["ip", "address", "*", "*", "secondary"]);
        assert_eq!(ls[0].children[1].line, 12);
    }
}
