//! Lexers for the supported config grammars. Tokens borrow from the input.
//!
//! * `indent`: nesting by indentation (IOS, NX-OS, EOS, and most CLI-style vendors).
//! * `braces`: nesting by `{ }` with `;`-terminated statements (Junos structured config).

/// One config statement and the statements nested under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node<'a> {
    pub tokens: Vec<&'a str>,
    pub children: Vec<Node<'a>>,
    /// 1-based source line of the statement (0 if unknown).
    pub line: usize,
}

impl<'a> Node<'a> {
    pub fn line_text(&self) -> String {
        self.tokens.join(" ")
    }
}

/// An owned node, produced by rendering and by the unmanaged report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedNode {
    pub tokens: Vec<String>,
    pub children: Vec<OwnedNode>,
    /// Rendered from a block model (so IOS-style separators apply even when empty).
    pub block: bool,
}

impl OwnedNode {
    pub fn leaf(tokens: Vec<String>) -> Self {
        OwnedNode { tokens, children: Vec::new(), block: false }
    }
    pub fn with_children(tokens: Vec<String>, children: Vec<OwnedNode>) -> Self {
        OwnedNode { tokens, children, block: true }
    }
    pub fn line(&self) -> String {
        self.tokens.join(" ")
    }
    pub fn from_node(n: &Node<'_>) -> Self {
        OwnedNode {
            tokens: n.tokens.iter().map(|t| t.to_string()).collect(),
            children: n.children.iter().map(OwnedNode::from_node).collect(),
            block: !n.children.is_empty(),
        }
    }
    /// Every leaf path, e.g. `router bgp 65000 > neighbor 10.0.0.1 > bfd`.
    pub fn leaf_paths(nodes: &[OwnedNode]) -> Vec<String> {
        fn go(n: &OwnedNode, prefix: &mut Vec<String>, out: &mut Vec<String>) {
            prefix.push(n.line());
            if n.children.is_empty() {
                out.push(prefix.join(" > "));
            } else {
                for c in &n.children {
                    go(c, prefix, out);
                }
            }
            prefix.pop();
        }
        let mut out = Vec::new();
        for n in nodes {
            go(n, &mut Vec::new(), &mut out);
        }
        out
    }
}

struct Frame<'a> {
    indent: usize,
    tokens: Vec<&'a str>,
    children: Vec<Node<'a>>,
    line: usize,
}

fn close<'a>(stack: &mut Vec<Frame<'a>>) {
    let done = stack.pop().unwrap();
    stack.last_mut().unwrap().children.push(Node { tokens: done.tokens, children: done.children, line: done.line });
}

/// Indentation grammar. `skip` drops comment/banner lines (given the trimmed line).
pub fn lex_indent<'a>(text: &'a str, skip: &dyn Fn(&str) -> bool) -> Vec<Node<'a>> {
    let mut stack: Vec<Frame<'a>> = vec![Frame { indent: usize::MAX, tokens: Vec::new(), children: Vec::new(), line: 0 }];
    for (i, raw) in text.split('\n').enumerate() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed_start = raw.trim_start_matches(' ');
        let indent = raw.len() - trimmed_start.len();
        let line = trimmed_start.trim_end();
        if line.is_empty() || skip(line) {
            continue;
        }
        while stack.len() > 1 && stack.last().unwrap().indent >= indent {
            close(&mut stack);
        }
        stack.push(Frame { indent, tokens: split_tokens(line), children: Vec::new(), line: i + 1 });
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().unwrap().children
}

/// Whitespace split that keeps `{{ ... }}` placeholders and `"quoted strings"` as one token.
/// Quotes are stripped; a token containing whitespace is therefore a quoted string.
fn split_tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        if b[i] == b'"' {
            if let Some(end) = line[i + 1..].find('"') {
                out.push(&line[i + 1..i + 1 + end]);
                i = i + 1 + end + 1;
                continue;
            }
        }
        if line[i..].starts_with("{{") {
            if let Some(end) = line[i..].find("}}") {
                out.push(&line[i..i + end + 2]);
                i += end + 2;
                continue;
            }
        }
        while i < b.len() && b[i] != b' ' && b[i] != b'\t' {
            i += 1;
        }
        out.push(&line[start..i]);
    }
    out
}

/// Brace grammar (Junos-style): `header { ... }` blocks and `statement;` leaves.
/// `#` and `/* */` comments are dropped; a bare newline also ends a statement, which real
/// configs never rely on but templates may.
pub fn lex_braces<'a>(text: &'a str) -> Vec<Node<'a>> {
    let mut stack: Vec<Frame<'a>> = vec![Frame { indent: 0, tokens: Vec::new(), children: Vec::new(), line: 0 }];
    let mut cur: Vec<&'a str> = Vec::new();
    let mut cur_line = 0;
    let b = text.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let flush_leaf = |cur: &mut Vec<&'a str>, cur_line: usize, stack: &mut Vec<Frame<'a>>| {
        if !cur.is_empty() {
            let toks = std::mem::take(cur);
            stack.last_mut().unwrap().children.push(Node { tokens: toks, children: Vec::new(), line: cur_line });
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => { flush_leaf(&mut cur, cur_line, &mut stack); line += 1; i += 1; }
            b' ' | b'\t' | b'\r' => i += 1,
            b';' => { flush_leaf(&mut cur, cur_line, &mut stack); i += 1; }
            b'{' if !text[i..].starts_with("{{") => {
                let toks = std::mem::take(&mut cur);
                stack.push(Frame { indent: 0, tokens: toks, children: Vec::new(), line: cur_line });
                i += 1;
            }
            b'}' if !text[i..].starts_with("}}") => {
                flush_leaf(&mut cur, cur_line, &mut stack);
                if stack.len() > 1 { close(&mut stack); }
                i += 1;
            }
            b'#' => { while i < b.len() && b[i] != b'\n' { i += 1; } }
            b'/' if text[i..].starts_with("/*") => {
                match text[i + 2..].find("*/") {
                    Some(end) => { line += text[i..i + 2 + end].matches('\n').count(); i += 2 + end + 2; }
                    None => i = b.len(),
                }
            }
            b'"' => {
                if cur.is_empty() { cur_line = line; }
                let end = text[i + 1..].find('"').map(|e| i + 1 + e).unwrap_or(b.len());
                cur.push(&text[i + 1..end]);
                i = end + 1;
            }
            _ => {
                if cur.is_empty() { cur_line = line; }
                let start = i;
                if text[i..].starts_with("{{") {
                    let end = text[i..].find("}}").map(|e| i + e + 2).unwrap_or(b.len());
                    cur.push(&text[start..end]);
                    i = end;
                    continue;
                }
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b';' | b'{' | b'}') {
                    i += 1;
                }
                cur.push(&text[start..i]);
            }
        }
    }
    flush_leaf(&mut cur, cur_line, &mut stack);
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().unwrap().children
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nests_by_indent() {
        let nodes = lex_indent("a\n b\n  c\n d\ne\n", &|_| false);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].children.len(), 2);
        assert_eq!(nodes[0].children[0].children[0].tokens, vec!["c"]);
        assert_eq!(nodes[1].line, 5);
    }

    #[test]
    fn skips_and_dedents() {
        let nodes = lex_indent("router bgp 1\n  neighbor x\n    remote-as 2\n!\n  neighbor y\nhostname h\n", &|l| l.starts_with('!'));
        assert_eq!(nodes[0].children.len(), 2);
        assert_eq!(nodes[1].tokens, vec!["hostname", "h"]);
    }

    #[test]
    fn quotes_and_placeholders_are_single_tokens() {
        assert_eq!(split_tokens(r#"description "to core" {{ x }} y"#), vec!["description", "to core", "{{ x }}", "y"]);
    }

    #[test]
    fn braces() {
        let t = "## Last commit\nsystem {\n    host-name r1;\n    /* note */\n}\nprotocols {\n    bgp {\n        group EXT {\n            neighbor 10.1.0.1 { peer-as 65001; description \"to peer one\"; }\n        }\n    }\n}\n";
        let nodes = lex_braces(t);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].tokens, vec!["system"]);
        assert_eq!(nodes[0].children[0].tokens, vec!["host-name", "r1"]);
        assert_eq!(nodes[0].children[0].line, 3);
        let nb = &nodes[1].children[0].children[0].children[0];
        assert_eq!(nb.tokens, vec!["neighbor", "10.1.0.1"]);
        assert_eq!(nb.children[1].tokens, vec!["description", "to peer one"]);
    }
}
