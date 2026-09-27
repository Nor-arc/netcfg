//! Golden configs: real device configs kept next to a set as its test oracle.
//!
//! `netcfg check --set S --golden dir/` parses every config under `dir/` with the set's device
//! model (the manifest's `root`), fails on strict errors, and writes each config's unmanaged
//! report to `dir/.unmanaged/<config>.txt`. With `--compare` it compares against the
//! committed reports instead of writing them, and fails on any difference: the CI gate for
//! template changes. Files and directories whose name starts with `.` are not configs.

use crate::engine::{Engine, Parsed};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Config files under `dir`, recursively, sorted; dot-files and dot-directories are skipped.
pub fn golden_files(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(p)? {
            let p = e?.path();
            if p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(true) { continue; }
            if p.is_dir() { walk(&p, out)?; } else { out.push(p); }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, &mut out).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
    out.sort();
    Ok(out)
}

/// Where a config's unmanaged report lives: `dir/.unmanaged/<relative path>.txt`.
pub fn report_path(dir: &Path, config: &Path) -> PathBuf {
    let rel = config.strip_prefix(dir).unwrap_or(config);
    let mut name = rel.as_os_str().to_owned();
    name.push(".txt");
    dir.join(".unmanaged").join(name)
}

/// One report: unmanaged paths, one per line.
pub fn report_text(p: &Parsed) -> String {
    p.unmanaged_paths().iter().map(|l| format!("{l}\n")).collect()
}

/// The result of checking one golden config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// Path relative to the golden directory.
    pub config: PathBuf,
    /// Number of unmanaged lines, when the config parsed.
    pub unmanaged: Option<usize>,
    /// What went wrong: a strict parse error, or a report that differs from the committed one.
    pub problem: Option<String>,
}

impl Engine {
    /// The device model: `model` if given, else the manifest's `root`.
    pub fn root_model<'a>(&'a self, model: Option<&'a str>) -> Result<&'a str> {
        model.or_else(|| self.set.as_ref().and_then(|m| m.root.as_deref()))
            .ok_or_else(|| Error("no device model: give --model, or declare `root:` in the set manifest".into()))
    }

    /// Parse every golden config under `dir` with `model`. Without `compare`, each report is
    /// written to `dir/.unmanaged/`; with it, each is compared to the committed report.
    pub fn check_goldens(&self, dir: &Path, model: &str, compare: bool) -> Result<Vec<Checked>> {
        let mut out = Vec::new();
        for path in golden_files(dir)? {
            let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
            let text = std::fs::read_to_string(&path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
            let parsed = match self.parse(model, &text) {
                Ok(p) => p,
                Err(e) => { out.push(Checked { config: rel, unmanaged: None, problem: Some(e.0) }); continue; }
            };
            let report = report_text(&parsed);
            let rp = report_path(dir, &path);
            let problem = if compare {
                match std::fs::read_to_string(&rp) {
                    Err(_) => Some(format!("no committed report at {} (run without --compare to write it)", rp.display())),
                    Ok(old) if old == report => None,
                    Ok(old) => Some(report_diff(&old, &report)),
                }
            } else {
                std::fs::create_dir_all(rp.parent().unwrap()).map_err(|e| Error(format!("{}: {e}", rp.display())))?;
                std::fs::write(&rp, &report).map_err(|e| Error(format!("{}: {e}", rp.display())))?;
                None
            };
            out.push(Checked { config: rel, unmanaged: Some(parsed.unmanaged_paths().len()), problem });
        }
        Ok(out)
    }
}

/// Lines only in the committed report (`-`) and only in the new one (`+`).
fn report_diff(old: &str, new: &str) -> String {
    let (o, n): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let mut out = vec!["unmanaged report differs from the committed one:".to_string()];
    out.extend(o.iter().filter(|l| !n.contains(l)).map(|l| format!("- {l}")));
    out.extend(n.iter().filter(|l| !o.contains(l)).map(|l| format!("+ {l}")));
    if out.len() == 1 { out.push("(same lines, different order)".into()); }
    out.join("\n")
}
