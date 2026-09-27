//! `netcfg suggest`: proposals for modelling what a config's unmanaged report contains.
//!
//! Unmanaged leaf lines are grouped under their (generalized) ancestors by literal prefix:
//! the tokens up to the first token that varies across occurrences. Each group gets a
//! proposed template line and field declaration:
//!
//! ```text
//! router bgp 65000 > neighbor * > bfd          (412x)  bfd [[ bfd ]]               bfd: flag
//! router bgp 65000 > neighbor * > password 3 * (390x)  password 3 {{ password }}   password: string?
//! ```
//!
//! Heuristics: a constant line is a flag; one varying trailing token is `string?` (`int?`
//! when all are numbers, `ipv4?`/`ipv6?` when all are addresses); several varying tokens are
//! `phrase?`. Where lines branch, words made of lowercase letters and `-` are keywords (each
//! gets its own group); anything else is a value. A value followed by more words (a key, as
//! in EOS `neighbor X bfd`) becomes `*` and grouping continues after it. Groups whose lines
//! an `@ignore` already covers are marked. Nothing is edited.

use crate::engine::{ignore_matches, Engine};
use crate::lexer::OwnedNode;
use crate::Result;

/// One proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Ancestors and the line's shape: `router bgp 65000 > neighbor * > password 3 *`.
    pub path: String,
    pub count: usize,
    pub template_line: String,
    pub field: String,
    /// Every line of the group is already covered by an `@ignore`.
    pub ignored: bool,
}

fn keyword(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

fn camel(words: &[String]) -> String {
    let mut out = String::new();
    for w in words.iter().filter(|w| *w != "*" && !w.chars().all(|c| c.is_ascii_digit())) {
        for (i, part) in w.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).enumerate() {
            let part = part.to_ascii_lowercase();
            if out.is_empty() && i == 0 { out.push_str(&part); } else {
                let mut cs = part.chars();
                if let Some(c) = cs.next() { out.push(c.to_ascii_uppercase()); out.extend(cs); }
            }
        }
    }
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_alphabetic()) { format!("value{out}") } else { out }
}

/// A leaf line as grouped (keys replaced by `*`) and as it was.
type Line = (Vec<String>, Vec<String>);

/// A group of leaf lines sharing ancestors and a literal prefix.
struct Group {
    prefix: Vec<String>,
    /// The varying tails; empty for a constant (flag) group.
    tails: Vec<Vec<String>>,
    /// The original lines, for `@ignore` matching.
    lines: Vec<Vec<String>>,
}

/// Split lines (all sharing `prefix` up to `depth`) into groups.
fn split(lines: Vec<Line>, depth: usize, prefix: Vec<String>, out: &mut Vec<Group>) {
    let (ended, rest): (Vec<Line>, Vec<Line>) = lines.into_iter().partition(|(l, _)| l.len() <= depth);
    if !ended.is_empty() { out.push(Group { prefix: prefix.clone(), tails: Vec::new(), lines: ended.into_iter().map(|(_, o)| o).collect() }); }
    if rest.is_empty() { return; }
    let mut keys: Vec<String> = rest.iter().map(|(l, _)| l[depth].clone()).collect();
    keys.sort();
    keys.dedup();
    // Every line ends right after this token, each with a different one: values.
    let unique_ends = rest.iter().all(|(l, _)| l.len() == depth + 1) && keys.len() == rest.len();
    let literal = match keys.as_slice() {
        // Constant across several occurrences, or the command word: a literal. A single
        // occurrence shows no variation, so it is a literal only if it looks like a keyword.
        [k] => rest.len() > 1 || depth == 0 || keyword(k),
        _ => (depth == 0 || keys.iter().all(|k| keyword(k))) && keys.len() <= 8 && !unique_ends,
    };
    if literal {
        for k in keys {
            let sub: Vec<Line> = rest.iter().filter(|(l, _)| l[depth] == k).cloned().collect();
            let mut p = prefix.clone();
            p.push(k);
            split(sub, depth + 1, p, out);
        }
        return;
    }
    // A value position. Several values, each followed by a keyword that repeats: a key (EOS
    // `neighbor X bfd`); keep grouping after it.
    let mut next: Vec<&str> = rest.iter().filter_map(|(l, _)| l.get(depth + 1).map(String::as_str)).collect();
    let continues = next.len() == rest.len();
    next.sort();
    next.dedup();
    if keys.len() > 1 && continues && next.iter().all(|t| keyword(t)) && next.len() < rest.len() {
        let mut p = prefix;
        p.push("*".into());
        let starred = rest.into_iter().map(|(mut l, o)| { l[depth] = "*".into(); (l, o) }).collect();
        split(starred, depth + 1, p, out);
        return;
    }
    out.push(Group { prefix, tails: rest.iter().map(|(l, _)| l[depth..].to_vec()).collect(), lines: rest.into_iter().map(|(_, o)| o).collect() });
}

/// Generalize ancestor lines level by level: tokens that vary between siblings that start
/// with the same word become `*`.
fn generalize(paths: &mut [(Vec<Vec<String>>, Vec<String>)]) {
    let depth = paths.iter().map(|(a, _)| a.len()).max().unwrap_or(0);
    for level in 0..depth {
        let mut groups: Vec<(Vec<Vec<String>>, String, Vec<usize>)> = Vec::new();
        for (i, (anc, _)) in paths.iter().enumerate() {
            if anc.len() <= level { continue; }
            let key = (anc[..level].to_vec(), anc[level].first().cloned().unwrap_or_default());
            match groups.iter_mut().find(|(a, f, _)| *a == key.0 && *f == key.1) {
                Some(g) => g.2.push(i),
                None => groups.push((key.0, key.1, vec![i])),
            }
        }
        for (_, _, members) in groups {
            let lines: Vec<Vec<String>> = members.iter().map(|&i| paths[i].0[level].clone()).collect();
            let min = lines.iter().map(Vec::len).min().unwrap_or(0);
            let mut g: Vec<String> = (0..min).map(|p| if lines.iter().all(|l| l[p] == lines[0][p]) { lines[0][p].clone() } else { "*".into() }).collect();
            if lines.iter().any(|l| l.len() != min) && g.last().map(|t| t != "*").unwrap_or(true) { g.push("*".into()); }
            for &i in &members { paths[i].0[level] = g.clone(); }
        }
    }
}

impl Engine {
    /// Proposals for the unmanaged lines of `text` parsed as `model`, most frequent first.
    pub fn suggest(&self, model: &str, text: &str) -> Result<Vec<Suggestion>> {
        let parsed = self.parse(model, text)?;
        Ok(self.suggest_from(&parsed.unmanaged))
    }

    /// Proposals for an unmanaged report.
    pub fn suggest_from(&self, unmanaged: &[OwnedNode]) -> Vec<Suggestion> {
        fn leaves(n: &OwnedNode, anc: &mut Vec<Vec<String>>, out: &mut Vec<(Vec<Vec<String>>, Vec<String>)>) {
            let toks: Vec<String> = n.tokens.iter().flat_map(|t| if t.contains(' ') { vec![format!("\"{t}\"")] } else { vec![t.clone()] }).collect();
            if n.children.is_empty() { out.push((anc.clone(), toks)); return; }
            anc.push(toks);
            for c in &n.children { leaves(c, anc, out); }
            anc.pop();
        }
        let mut paths = Vec::new();
        for n in unmanaged { leaves(n, &mut Vec::new(), &mut paths); }
        generalize(&mut paths);
        // Group leaves by their generalized ancestors, in first-seen order.
        // Leaves grouped by their generalized ancestors, in first-seen order.
        type Lines = Vec<Vec<String>>;
        let mut by_anc: Vec<(Lines, Lines)> = Vec::new();
        for (anc, leaf) in paths {
            match by_anc.iter_mut().find(|(a, _)| *a == anc) {
                Some((_, ls)) => ls.push(leaf),
                None => by_anc.push((anc, vec![leaf])),
            }
        }
        let ignores: Vec<&Vec<String>> = self.models.values().flat_map(|m| m.ignore_at.iter().map(|(p, _)| p)).collect();
        let mut out = Vec::new();
        for (anc, leaves) in by_anc {
            let mut groups = Vec::new();
            split(leaves.into_iter().map(|l| (l.clone(), l)).collect(), 0, Vec::new(), &mut groups);
            let anc_text: Vec<String> = anc.iter().map(|l| l.join(" ")).collect();
            for g in groups {
                let name = camel(&g.prefix);
                let lits: Vec<String> = g.prefix.iter().map(|t| if t == "*" { "{{ key }}".to_string() } else { t.clone() }).collect();
                let (shape, template_line, field) = if g.tails.is_empty() {
                    (g.prefix.join(" "), format!("{} [[ {name} ]]", lits.join(" ")), format!("{name}: flag"))
                } else {
                    let ty = if g.tails.iter().all(|t| t.len() == 1) {
                        let all = |f: &dyn Fn(&str) -> bool| g.tails.iter().all(|t| f(&t[0]));
                        if all(&|w| w.parse::<i64>().is_ok()) { "int" }
                        else if all(&|w| w.parse::<std::net::Ipv4Addr>().is_ok()) { "ipv4" }
                        else if all(&|w| w.parse::<std::net::Ipv6Addr>().is_ok()) { "ipv6" }
                        else { "string" }
                    } else { "phrase" };
                    (format!("{} *", g.prefix.join(" ")).trim_start().to_string(), format!("{} {{{{ {name} }}}}", lits.join(" ")).trim_start().to_string(), format!("{name}: {ty}?"))
                };
                let path = anc_text.iter().cloned().chain(std::iter::once(shape)).collect::<Vec<_>>().join(" > ");
                let ignored = g.lines.iter().all(|l| {
                    let toks: Vec<&str> = l.iter().map(String::as_str).collect();
                    ignores.iter().any(|p| ignore_matches(p, &toks))
                });
                out.push(Suggestion { path, count: g.lines.len(), template_line, field, ignored });
            }
        }
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.path.cmp(&b.path)));
        out
    }
}

/// Aligned text, one suggestion per line.
pub fn format(suggestions: &[Suggestion]) -> String {
    let w_path = suggestions.iter().map(|s| s.path.len()).max().unwrap_or(0);
    let w_count = suggestions.iter().map(|s| s.count.to_string().len()).max().unwrap_or(0) + 3;
    let w_line = suggestions.iter().map(|s| s.template_line.len()).max().unwrap_or(0);
    suggestions.iter().map(|s| {
        let count = format!("({}x)", s.count);
        let line = format!("{:w_path$}  {count:>w_count$}  {:w_line$}  {}{}", s.path, s.template_line, s.field, if s.ignored { "  (covered by @ignore)" } else { "" });
        format!("{}\n", line.trim_end())
    }).collect()
}
