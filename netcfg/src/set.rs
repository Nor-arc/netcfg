//! Template sets: a manifest names the files of a set, its dialect and its version.
//!
//! ```text
//! set nxos
//!   dialect: nxos
//!   version: 2026.09.1
//!   include: ../common/routemap.nct
//!   files: *.nct            # the default
//!   root: Device            # the device model, for `netcfg check`
//! ```
//!
//! Paths are relative to the manifest. `files` and `include` take comma-separated glob
//! patterns (`*`, `?`, and `**` for any depth); `include` may repeat, and each pattern must
//! match at least one file. Test files (`*.test.nct`) and the manifest itself are never
//! template files. Without a manifest, a directory is loaded as a set (`Engine::load_dir`).

use crate::engine::{is_template_file, Engine};
use crate::model::{self, SetDef};
use crate::{Error, Result};
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

/// A resolved `set` section.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub path: PathBuf,
    pub dialect: Option<String>,
    pub version: Option<String>,
    pub root: Option<String>,
    pub files: Vec<String>,
    pub includes: Vec<String>,
}

const PROPS: &[&str] = &["dialect", "version", "include", "files", "root"];

impl Manifest {
    fn from_def(path: &Path, d: &SetDef) -> Result<Manifest> {
        let at = |ln: usize| format!("{}:{ln}", path.display());
        let mut m = Manifest { name: d.name.clone(), path: path.to_path_buf(), dialect: None, version: None, root: None, files: Vec::new(), includes: Vec::new() };
        let list = |v: &str| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
        for (k, v, ln) in &d.props {
            let once = |slot: &Option<String>| if slot.is_some() { Err(Error(format!("{}: `{k}` is given twice", at(*ln)))) } else { Ok(()) };
            match k.as_str() {
                "dialect" => { once(&m.dialect)?; m.dialect = Some(v.clone()); }
                "version" => { once(&m.version)?; m.version = Some(v.clone()); }
                "root" => { once(&m.root)?; m.root = Some(v.clone()); }
                "files" => m.files.extend(list(v)),
                "include" => m.includes.extend(list(v)),
                other => return Err(Error(format!("{}: unknown set property `{other}` (known: {})", at(*ln), PROPS.join(", ")))),
            }
        }
        if m.files.is_empty() { m.files.push("*.nct".into()); }
        Ok(m)
    }

    /// Read `path` as a manifest; `Ok(None)` if the file has no `set` section.
    pub fn read(path: &Path) -> Result<Option<(Manifest, model::File)>> {
        let text = std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        let file = model::parse(&path.display().to_string(), &text)?;
        match &file.set {
            Some(d) => Ok(Some((Manifest::from_def(path, d)?, file))),
            None => Ok(None),
        }
    }

    /// Template files of the set, in load order: `files` matches, then `include`s.
    pub fn template_paths(&self) -> Result<Vec<PathBuf>> {
        let base = self.path.parent().unwrap_or(Path::new("."));
        let me = canonical(&self.path);
        let mut out: Vec<PathBuf> = Vec::new();
        let push = |p: PathBuf, out: &mut Vec<PathBuf>| {
            if canonical(&p) != me && !out.iter().any(|q| canonical(q) == canonical(&p)) { out.push(p); }
        };
        for pat in &self.files {
            for p in glob(base, pat)? { if is_template_file(&p) { push(p, &mut out); } }
        }
        for pat in &self.includes {
            let found = glob(base, pat)?;
            if found.is_empty() { return Err(Error(format!("{}: set {}: include `{pat}` matches no file", self.path.display(), self.name))); }
            for p in found { push(p, &mut out); }
        }
        Ok(out)
    }
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// `*` and `?` within one path component.
fn wildcard(pat: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) { pi += 1; si += 1; }
        else if pi < p.len() && p[pi] == '*' { star = Some(pi); mark = si; pi += 1; }
        else if let Some(st) = star { pi = st + 1; mark += 1; si = mark; }
        else { return false; }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    pi == p.len()
}

/// Files under `base` matching a relative glob (`*.nct`, `../common/*.nct`, `**/*.nct`).
/// A pattern without wildcards names one file, which must exist to match.
pub fn glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    fn go(dir: &Path, parts: &[&str], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let Some((first, rest)) = parts.split_first() else { return Ok(()) };
        if *first == "**" {
            go(dir, rest, out)?;
            for e in std::fs::read_dir(dir)? {
                let p = e?.path();
                if p.is_dir() { go(&p, parts, out)?; }
            }
            return Ok(());
        }
        if !first.contains(['*', '?']) {
            let p = dir.join(first);
            if rest.is_empty() { if p.is_file() { out.push(p); } } else if p.is_dir() { go(&p, rest, out)?; }
            return Ok(());
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>()?;
        entries.sort();
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !wildcard(first, name) { continue; }
            if rest.is_empty() { if p.is_file() { out.push(p); } } else if p.is_dir() { go(&p, rest, out)?; }
        }
        Ok(())
    }
    let parts: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let mut out = Vec::new();
    go(base, &parts, &mut out).map_err(|e| Error(format!("{}: {pattern}: {e}", base.display())))?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// Manifests directly in `dir` (not recursive).
fn manifests_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in glob(dir, "*.nct")? {
        if is_template_file(&p) && Manifest::read(&p)?.is_some() { out.push(p); }
    }
    Ok(out)
}

impl Engine {
    /// Load the set described by the manifest at `path`.
    pub fn load_set(path: &Path) -> Result<Engine> {
        let (m, own) = Manifest::read(path)?.ok_or_else(|| Error(format!("{}: not a set manifest (no `set NAME` section)", path.display())))?;
        let paths = m.template_paths()?;
        let mut files = vec![own];
        files.extend(Engine::read_files(&paths)?);
        let mut e = Engine::build(&files, m.dialect.as_deref()).map_err(|e| Error(format!("set {} ({}): {}", m.name, path.display(), e.0)))?;
        if let Some(d) = &m.dialect {
            if &e.dialect.name != d {
                return Err(Error(format!("{}: set {} names dialect `{d}`, but its files declare `dialect {}`", path.display(), m.name, e.dialect.name)));
            }
        }
        if let Some(r) = &m.root {
            if e.model(r).is_none() { return Err(Error(format!("{}: set {}: root `{r}` is not a model (known: {})", path.display(), m.name, e.model_names().join(", ")))); }
        }
        e.templates_version = m.version.clone();
        e.set = Some(m);
        Ok(e)
    }

    /// Every set under `dir`: each manifest found recursively, by set name.
    pub fn load_all(dir: &Path) -> Result<IndexMap<String, Engine>> {
        let mut found = Vec::new();
        for p in glob(dir, "**/*.nct")? {
            if is_template_file(&p) && Manifest::read(&p)?.is_some() { found.push(p); }
        }
        let mut out: IndexMap<String, Engine> = IndexMap::new();
        let mut errors = Vec::new();
        for p in found {
            match Engine::load_set(&p) {
                Ok(e) => {
                    let name = e.set.as_ref().unwrap().name.clone();
                    if let Some(prev) = out.get(&name) {
                        errors.push(format!("set `{name}` is defined twice: {} and {}", prev.set.as_ref().unwrap().path.display(), p.display()));
                    } else {
                        out.insert(name, e);
                    }
                }
                Err(e) => errors.push(e.0),
            }
        }
        if !errors.is_empty() { return Err(Error(errors.join("\n"))); }
        Ok(out)
    }

    /// A manifest file, or a directory: its manifest if it has exactly one directly in it,
    /// otherwise every template file under it (with `fallback` as the dialect if none is
    /// declared).
    pub fn load_path(path: &Path, fallback: Option<&str>) -> Result<Engine> {
        if path.is_file() { return Engine::load_set(path); }
        match manifests_in(path)?.as_slice() {
            [one] => Engine::load_set(one),
            [] => Engine::load_dir_files(path, fallback),
            many => Err(Error(format!("{}: several set manifests ({}); name one with --set", path.display(), many.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn wildcards() {
        assert!(wildcard("*.nct", "bgp.nct"));
        assert!(!wildcard("*.nct", "bgp.ttp"));
        assert!(wildcard("b?p*", "bgp.nct"));
        assert!(wildcard("*", ""));
        assert!(!wildcard("a*b", "acb.c"));
    }
}
