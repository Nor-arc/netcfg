//! `netcfg test`: template tests in `*.test.nct` files next to the templates.
//!
//! ```text
//! test "neighbor with maximum-routes"
//!   model: EosNeighbor
//!   config:
//!     neighbor 1.1.1.1 remote-as 65431
//!     neighbor 1.1.1.1 maximum-routes 1200 warning-only
//!   expect:
//!     peer: 1.1.1.1
//!     remoteAs: 65431
//!     maximumRoutes: {limit: 1200, action: warning-only}
//!   roundtrip: true
//!
//! test "unknown modifier fails"
//!   model: EosNeighbor
//!   config:
//!     neighbor 1.1.1.1 maximum-routes 1200 loudly
//!   fails: starts like a managed line
//! ```
//!
//! Properties: `model` and `config` (required); `expect` (YAML data, compared exactly in
//! canonical form: flags and defaulted fields at their default, and empty collections, may be
//! left out; every other key must match, and missing keys must be missing); `unmanaged` (YAML list of unmanaged
//! paths, exactly); `roundtrip` (render the parse and parse it again: same data, nothing
//! unmanaged); `render` (the canonical rendering of the parse, exactly); `fails` (parsing
//! must fail with a message containing this text). A block property is written `key:` (or
//! `key: |`) with the block indented below it; blank lines inside a block are kept.

use crate::engine::Engine;
use crate::value::{Record, Value};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct TestCase {
    pub name: String,
    pub source: String,
    pub line: usize,
    pub model: String,
    pub config: String,
    pub expect: Option<Value>,
    pub unmanaged: Option<Vec<String>>,
    pub roundtrip: bool,
    pub render: Option<String>,
    pub fails: Option<String>,
}

/// The outcome of one test: `Ok(())` or what went wrong.
pub type Outcome = std::result::Result<(), String>;

const KEYS: &[&str] = &["model", "config", "expect", "unmanaged", "roundtrip", "render", "fails"];

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// Dedent a block, keeping blank lines inside it and its relative indentation.
fn block(lines: &[&str]) -> String {
    let mut lines = lines.to_vec();
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) { lines.pop(); }
    let min = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| indent(l)).min().unwrap_or(0);
    let mut out = String::new();
    for l in lines {
        if l.trim().is_empty() { out.push('\n'); } else { out.push_str(&l[min..]); out.push('\n'); }
    }
    out
}

/// Parse a `*.test.nct` file.
pub fn parse(source: &str, text: &str) -> Result<Vec<TestCase>> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    let mut cases = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.trim().is_empty() || (indent(l) == 0 && l.starts_with('#')) { i += 1; continue; }
        let ln = i + 1;
        let name = match l.strip_prefix("test ").map(str::trim).and_then(|n| n.strip_prefix('"')).and_then(|n| n.strip_suffix('"')) {
            Some(n) if indent(l) == 0 => n.to_string(),
            _ => { errors.push(format!("{source}:{ln}: expected `test \"name\"`")); i += 1; continue; }
        };
        i += 1;
        let start = i;
        while i < lines.len() && (lines[i].trim().is_empty() || indent(lines[i]) > 0) { i += 1; }
        let body = &lines[start..i];
        match parse_case(source, &name, ln, start, body) {
            Ok(c) => cases.push(c),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() { Ok(cases) } else { Err(Error(errors.join("\n"))) }
}

fn parse_case(source: &str, name: &str, ln: usize, first: usize, body: &[&str]) -> std::result::Result<TestCase, String> {
    let at = |k: usize| format!("{source}:{}: test \"{name}\"", first + k + 1);
    let mut c = TestCase { name: name.to_string(), source: source.to_string(), line: ln, ..Default::default() };
    let Some(prop) = body.iter().find(|l| !l.trim().is_empty()).map(|l| indent(l)) else {
        return Err(format!("{source}:{ln}: test \"{name}\" is empty"));
    };
    let mut k = 0;
    let mut seen: Vec<String> = Vec::new();
    while k < body.len() {
        let l = body[k];
        if l.trim().is_empty() || l.trim_start().starts_with('#') && indent(l) == prop { k += 1; continue; }
        if indent(l) != prop { return Err(format!("{}: unexpected indentation (properties are indented {prop} spaces)", at(k))); }
        let (key, inline) = l.trim().split_once(':').map(|(a, b)| (a.trim(), b.trim())).ok_or_else(|| format!("{}: expected `key: value` or `key:` with an indented block", at(k)))?;
        // `key: |` is `key:` with a block, as in YAML.
        let inline = if inline == "|" { "" } else { inline };
        let here = k;
        k += 1;
        let from = k;
        while k < body.len() && (body[k].trim().is_empty() || indent(body[k]) > prop) { k += 1; }
        // Blank lines after the block belong to no property.
        while k > from && body[k - 1].trim().is_empty() { k -= 1; }
        let text = if inline.is_empty() { block(&body[from..k]) } else if from == k { inline.to_string() } else {
            return Err(format!("{}: `{key}` has both an inline value and a block", at(here)));
        };
        if seen.iter().any(|s| s == key) { return Err(format!("{}: `{key}` is given twice", at(here))); }
        seen.push(key.to_string());
        let yaml = |t: &str| -> std::result::Result<serde_json::Value, String> { serde_yaml::from_str(t).map_err(|e| format!("{}: `{key}` is not valid YAML: {e}", at(here))) };
        match key {
            "model" => c.model = text.trim().to_string(),
            "config" => c.config = if inline.is_empty() { text } else { format!("{text}\n") },
            "render" => c.render = Some(if inline.is_empty() { text } else { format!("{text}\n") }),
            "expect" => c.expect = Some(Value::from_json(&yaml(&text)?)),
            "unmanaged" => {
                let j = yaml(&text)?;
                let list = j.as_array().and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<Vec<_>>>())
                    .ok_or_else(|| format!("{}: `unmanaged` must be a list of paths", at(here)))?;
                c.unmanaged = Some(list);
            }
            "roundtrip" => c.roundtrip = match text.trim() { "true" => true, "false" => false, v => return Err(format!("{}: `roundtrip` must be true or false, got `{v}`", at(here))) },
            "fails" => c.fails = Some(text.trim().to_string()),
            other => return Err(format!("{}: unknown property `{other}` (known: {})", at(here), KEYS.join(", "))),
        }
    }
    if c.model.is_empty() { return Err(format!("{source}:{ln}: test \"{name}\" needs `model:`")); }
    if !seen.iter().any(|s| s == "config") { return Err(format!("{source}:{ln}: test \"{name}\" needs `config:`")); }
    if c.fails.is_some() && (c.expect.is_some() || c.unmanaged.is_some() || c.roundtrip || c.render.is_some()) {
        return Err(format!("{source}:{ln}: test \"{name}\": `fails` cannot be combined with expect, unmanaged, roundtrip or render"));
    }
    Ok(c)
}

impl Engine {
    /// `value` without flags and defaulted fields that are at their defaults, and without empty
    /// collections (recursively): the form `expect:` is compared in. These are exactly the
    /// fields canonical rendering writes nothing for.
    pub fn canonical_data(&self, model: &str, value: &Value) -> Result<Value> {
        let m = self.model(model).ok_or_else(|| Error(format!("unknown model `{model}`")))?;
        let Some(rec) = value.as_record() else { return Ok(value.clone()) };
        let mut out = Record::new();
        for (k, v) in rec {
            let Some((i, f)) = m.fields.iter().enumerate().find(|(_, f)| &f.name == k) else { out.insert(k.clone(), v.clone()); continue };
            if self.field_default(m, i).as_ref() == Some(v) { continue; }
            if f.kind == crate::model::Kind::Many && v.as_list().map(|l| l.is_empty()).unwrap_or(false) { continue; }
            let v = match (f.kind.is_nested(), v) {
                (true, Value::List(l)) => Value::List(l.iter().map(|x| self.canonical_data(&f.type_spec, x)).collect::<Result<_>>()?),
                (true, Value::Record(_)) => self.canonical_data(&f.type_spec, v)?,
                _ => v.clone(),
            };
            out.insert(k.clone(), v);
        }
        Ok(Value::Record(out))
    }

    /// Run one test.
    pub fn run_test(&self, t: &TestCase) -> Outcome {
        let parsed = match (self.parse(&t.model, &t.config), &t.fails) {
            (Err(e), Some(frag)) => return if e.0.contains(frag.as_str()) { Ok(()) } else { Err(format!("error lacks `{frag}`: {}", e.0)) },
            (Ok(p), Some(frag)) => return Err(format!("expected an error containing `{frag}`, but it parsed: {}", p.value.to_json())),
            (Err(e), None) => return Err(format!("parse failed: {}", e.0)),
            (Ok(p), None) => p,
        };
        if let Some(want) = &t.expect {
            let got = self.canonical_data(&t.model, &parsed.value).map_err(|e| e.0)?;
            let want = self.canonical_data(&t.model, want).map_err(|e| e.0)?;
            if got != want {
                return Err(format!("parsed data differs\n  expected: {}\n  got:      {}", want.to_json(), got.to_json()));
            }
        }
        if let Some(want) = &t.unmanaged {
            let got = parsed.unmanaged_paths();
            if &got != want { return Err(format!("unmanaged lines differ\n  expected: {want:?}\n  got:      {got:?}")); }
        }
        if t.roundtrip || t.render.is_some() {
            let out = self.render(&t.model, &parsed.value).map_err(|e| format!("render failed: {}", e.0))?;
            if let Some(want) = &t.render {
                if &out != want { return Err(format!("rendering differs\n--- expected\n{want}--- got\n{out}")); }
            }
            if t.roundtrip {
                let again = self.parse(&t.model, &out).map_err(|e| format!("re-parse of the rendering failed: {}\n{out}", e.0))?;
                if again.value != parsed.value { return Err(format!("round trip changed the data\n  before: {}\n  after:  {}", parsed.value.to_json(), again.value.to_json())); }
                if !again.unmanaged.is_empty() { return Err(format!("round trip left unmanaged lines: {:?}", again.unmanaged_paths())); }
            }
        }
        Ok(())
    }

    /// Every `*.test.nct` file under `dir`, parsed and run in file order.
    pub fn run_tests_in(&self, dir: &Path) -> Result<Vec<(TestCase, Outcome)>> {
        let mut out = Vec::new();
        for p in test_files(dir)? {
            let text = std::fs::read_to_string(&p).map_err(|e| Error(format!("{}: {e}", p.display())))?;
            for t in parse(&p.display().to_string(), &text)? {
                let r = self.run_test(&t);
                out.push((t, r));
            }
        }
        Ok(out)
    }

    /// Where a set's tests live: the manifest's directory, else nothing (use a directory).
    pub fn tests_dir(&self) -> Option<PathBuf> {
        self.set.as_ref().and_then(|m| m.path.parent().map(Path::to_path_buf))
    }
}

/// `*.test.nct` files under `dir`, recursively, sorted.
pub fn test_files(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(crate::set::glob(dir, "**/*.test.nct")?)
}
