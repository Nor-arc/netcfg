//! `netcfg fmt`: rewrite `.nct` files to the current form without changing their meaning.
//!
//! * a bare `template` becomes `template NAME` (the immediately preceding model);
//! * by default each template is moved directly after its model when both are in the file.
//!
//! The file is treated as a list of top-level sections. Comment lines directly above a
//! section (no blank line between) travel with it; everything else keeps its place and its
//! spacing, so formatting an already formatted file changes nothing.

use crate::model::is_ident;

struct Chunk {
    /// Blank lines before the chunk (its leading comments included).
    blank_before: usize,
    lines: Vec<String>,
    /// `model`/`fragment` name declared, or template target, for sections that have one.
    model: Option<String>,
    template: Option<String>,
}

fn is_top(l: &str) -> bool {
    !l.is_empty() && !l.starts_with(' ') && !l.starts_with('\t')
}

/// Format one file's text. `move_templates: false` only renames bare templates.
pub fn format(text: &str, move_templates: bool) -> String {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    let mut chunks: Vec<Chunk> = Vec::new();
    let owned = |r: &[&str]| r.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let mut i = 0;
    let mut last_model: Option<String> = None;
    while i < lines.len() {
        let mut blank = 0;
        while i < lines.len() && lines[i].is_empty() { blank += 1; i += 1; }
        if i >= lines.len() { break; }
        let start = i;
        // Leading comments directly attached to the next section.
        while i < lines.len() && lines[i].starts_with('#') { i += 1; }
        if i >= lines.len() || lines[i].is_empty() || !is_top(lines[i]) {
            // A free-standing comment block (or stray indented lines): keep in place.
            while i < lines.len() && !lines[i].is_empty() && !is_top(lines[i]) { i += 1; }
            chunks.push(Chunk { blank_before: blank, lines: owned(&lines[start..i]), model: None, template: None });
            continue;
        }
        let header = lines[i];
        i += 1;
        // Body: indented lines, and blank lines followed by more indented lines.
        loop {
            let mut j = i;
            while j < lines.len() && lines[j].is_empty() { j += 1; }
            if j < lines.len() && j > i && !is_top(lines[j]) && !lines[j].is_empty() { i = j; continue; }
            if i < lines.len() && !lines[i].is_empty() && !is_top(lines[i]) { i += 1; continue; }
            break;
        }
        let mut chunk = Chunk { blank_before: blank, lines: owned(&lines[start..i]), model: None, template: None };
        let mut words = header.splitn(2, ' ');
        let (kw, rest) = (words.next().unwrap_or(""), words.next().unwrap_or("").trim());
        match kw {
            "model" | "fragment" if is_ident(rest) => { last_model = Some(rest.to_string()); chunk.model = last_model.clone(); }
            "template" if rest.is_empty() => {
                if let Some(m) = &last_model {
                    let at = chunk.lines.iter().position(|l| l == header).unwrap();
                    chunk.lines[at] = format!("template {m}");
                    chunk.template = Some(m.clone());
                }
            }
            "template" if is_ident(rest) => chunk.template = Some(rest.to_string()),
            _ => {}
        }
        chunks.push(chunk);
    }

    if move_templates {
        let models: Vec<String> = chunks.iter().filter_map(|c| c.model.clone()).collect();
        let mut out: Vec<Chunk> = Vec::new();
        let mut templates: Vec<Chunk> = Vec::new();
        for c in chunks {
            match &c.template {
                Some(t) if models.contains(t) => templates.push(c),
                _ => out.push(c),
            }
        }
        for t in templates {
            let name = t.template.clone().unwrap();
            let at = out.iter().position(|c| c.model.as_deref() == Some(name.as_str())).unwrap();
            out.insert(at + 1, Chunk { blank_before: 1, ..t });
        }
        chunks = out;
    }

    let mut s = String::new();
    for (k, c) in chunks.iter().enumerate() {
        if k > 0 { for _ in 0..c.blank_before.max(if c.template.is_some() || c.model.is_some() { 1 } else { 0 }) { s.push('\n'); } }
        else { for _ in 0..c.blank_before { s.push('\n'); } }
        for l in &c.lines { s.push_str(l); s.push('\n'); }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn names_and_moves_templates() {
        let src = "type a = \"x\" | \"y\"\n\n# The neighbor.\nmodel N\n  peer: key ip\n\nmodel B\n  n: [N]\n\ntemplate\n  router bgp\n    << n >>\n\n# N's lines\ntemplate N\n  neighbor {{ peer }}\n";
        let out = format(src, true);
        assert_eq!(out, "type a = \"x\" | \"y\"\n\n# The neighbor.\nmodel N\n  peer: key ip\n\n# N's lines\ntemplate N\n  neighbor {{ peer }}\n\nmodel B\n  n: [N]\n\ntemplate B\n  router bgp\n    << n >>\n");
        assert_eq!(format(&out, true), out, "idempotent");
        assert!(format(src, false).contains("model B\n  n: [N]\n\ntemplate B\n"));
    }

    #[test]
    fn keeps_blank_lines_inside_templates_and_foreign_templates() {
        let src = "template Elsewhere\n  x {{ y }}\n\n  z {{ w }}\nmodel M\n  a: int\n\ntemplate M\n  a {{ a }}\n";
        assert_eq!(format(src, true), "template Elsewhere\n  x {{ y }}\n\n  z {{ w }}\n\nmodel M\n  a: int\n\ntemplate M\n  a {{ a }}\n");
    }
}
