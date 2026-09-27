use clap::{Args, Parser, Subcommand, ValueEnum};
use netcfg::{Engine, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "netcfg", version, about = "Bidirectional network config templates")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format { Json, Yaml }

/// Where the templates come from: `--set path/to/set.nct`, or a leading TEMPLATES argument
/// (a set manifest, or a directory: its manifest if it has one, else every .nct file in it).
#[derive(Args)]
struct Src {
    /// Set manifest to load (replaces the TEMPLATES argument).
    #[arg(long, global = true)]
    set: Option<PathBuf>,
    /// Builtin dialect to use when the templates declare none (ios, nxos, eos, junos).
    #[arg(long, global = true)]
    dialect: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Load a set and report all errors. A directory holding several set manifests (at any
    /// depth, none directly in it) validates every set.
    Validate {
        #[arg(value_name = "TEMPLATES")] paths: Vec<PathBuf>,
        #[command(flatten)] src: Src,
    },
    /// Parse a running config into model data (JSON/YAML) plus the unmanaged report.
    Parse {
        #[arg(value_name = "[TEMPLATES] CONFIG", num_args = 1..=2, required = true)] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[arg(long, value_enum, default_value = "yaml")] format: Format,
        /// Print unmanaged lines (config the model doesn't cover) to stderr as paths.
        #[arg(long)] unmanaged: bool,
        #[command(flatten)] src: Src,
    },
    /// Render model data (JSON/YAML) to config text.
    Render {
        #[arg(value_name = "[TEMPLATES] DATA", num_args = 1..=2, required = true)] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[command(flatten)] src: Src,
    },
    /// Show how every field of a model is spelled in config (values, defaults, negations).
    Explain {
        #[arg(value_name = "[TEMPLATES]")] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[command(flatten)] src: Src,
    },
    /// Print the JSON Schema for a model's data.
    Schema {
        #[arg(value_name = "[TEMPLATES]")] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[command(flatten)] src: Src,
    },
    /// Print example YAML data for a model: every field, typed placeholders, comments.
    Skeleton {
        #[arg(value_name = "[TEMPLATES]")] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[command(flatten)] src: Src,
    },
    /// Check a data file (JSON/YAML) against a model without rendering; prints every error.
    ValidateData {
        #[arg(value_name = "[TEMPLATES] DATA", num_args = 1..=2, required = true)] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[command(flatten)] src: Src,
    },
    /// Rewrite .nct files to the current form: named templates, each placed after its model.
    Fmt {
        /// Files or directories (searched recursively for .nct/.ttp files).
        #[arg(required = true)] paths: Vec<PathBuf>,
        /// Only rename bare templates; don't move templates next to their models.
        #[arg(long)] keep_order: bool,
        /// Don't write; exit non-zero if any file would change.
        #[arg(long)] check: bool,
    },
    /// Time parse and render on a config.
    Bench {
        #[arg(value_name = "[TEMPLATES] CONFIG", num_args = 1..=2, required = true)] paths: Vec<PathBuf>,
        #[arg(long)] model: String,
        #[arg(long, default_value_t = 5)] runs: usize,
        #[command(flatten)] src: Src,
    },
}

fn warn(e: &Engine) {
    for w in &e.warnings { eprintln!("warning: {w}"); }
}

/// Load the engine from `--set` or the first of `paths`, and return the remaining paths,
/// which must be exactly `rest` (names for the usage message).
fn load(src: &Src, paths: &[PathBuf], rest: &[&str]) -> Result<(Engine, Vec<PathBuf>), String> {
    let usage = |got: usize| format!("expected {}{} argument(s), got {got}", if src.set.is_some() { "" } else { "TEMPLATES " }, rest.join(" "));
    let (e, remaining) = match &src.set {
        Some(set) => {
            if paths.len() != rest.len() { return Err(usage(paths.len())); }
            (Engine::load_set(set).map_err(|e| e.0)?, paths.to_vec())
        }
        None => {
            if paths.len() != rest.len() + 1 { return Err(usage(paths.len())); }
            (Engine::load_path(&paths[0], src.dialect.as_deref()).map_err(|e| e.0)?, paths[1..].to_vec())
        }
    };
    warn(&e);
    Ok((e, remaining))
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_data(path: &Path) -> Result<Value, String> {
    let text = read_text(path)?;
    let j: serde_json::Value = if path.extension().map(|x| x == "json").unwrap_or(false) {
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    };
    Ok(Value::from_json(&j))
}

fn template_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if p.is_dir() {
            for e in std::fs::read_dir(p)? { walk(&e?.path(), out)?; }
        } else if netcfg::engine::is_template_file(p) {
            out.push(p.to_path_buf());
        }
        Ok(())
    }
    let mut out = Vec::new();
    for p in paths {
        if p.is_file() { out.push(p.clone()); } else { walk(p, &mut out).map_err(|e| format!("{}: {e}", p.display()))?; }
    }
    out.sort();
    Ok(out)
}

fn describe(e: &Engine) -> String {
    let set = match &e.set {
        Some(m) => format!("set {} {}, ", m.name, e.templates_version()),
        None => String::new(),
    };
    format!("{set}dialect {} ({:?} grammar), {} model(s): {}", e.dialect.name, e.dialect.grammar, e.model_names().len(), e.model_names().join(", "))
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Validate { paths, src } => {
            // A directory of sets (manifests below it, none directly in it): validate each.
            if let (None, [dir]) = (&src.set, paths.as_slice()) {
                let direct = dir.is_dir() && netcfg::set::glob(dir, "*.nct").map_err(|e| e.0)?.iter()
                    .any(|p| netcfg::set::Manifest::read(p).ok().flatten().is_some());
                if dir.is_dir() && !direct {
                    let all = Engine::load_all(dir).map_err(|e| e.0)?;
                    if !all.is_empty() {
                        for e in all.values() { warn(e); println!("ok: {}", describe(e)); }
                        return Ok(());
                    }
                }
            }
            let (e, _) = load(&src, &paths, &[])?;
            println!("ok: {}", describe(&e));
            Ok(())
        }
        Cmd::Parse { paths, model, format, unmanaged, src } => {
            let (e, rest) = load(&src, &paths, &["CONFIG"])?;
            let p = e.parse(&model, &read_text(&rest[0])?).map_err(|e| e.0)?;
            let j = p.value.to_json();
            match format {
                Format::Json => println!("{}", serde_json::to_string_pretty(&j).unwrap()),
                Format::Yaml => print!("{}", serde_yaml::to_string(&j).unwrap()),
            }
            if unmanaged {
                for path in p.unmanaged_paths() { eprintln!("unmanaged: {path}"); }
            }
            Ok(())
        }
        Cmd::Render { paths, model, src } => {
            let (e, rest) = load(&src, &paths, &["DATA"])?;
            print!("{}", e.render(&model, &read_data(&rest[0])?).map_err(|e| e.0)?);
            Ok(())
        }
        Cmd::Explain { paths, model, src } => {
            let (e, _) = load(&src, &paths, &[])?;
            print!("{}", e.explain(&model).map_err(|e| e.0)?);
            Ok(())
        }
        Cmd::Schema { paths, model, src } => {
            let (e, _) = load(&src, &paths, &[])?;
            println!("{}", serde_json::to_string_pretty(&e.schema(&model).map_err(|e| e.0)?).unwrap());
            Ok(())
        }
        Cmd::Skeleton { paths, model, src } => {
            let (e, _) = load(&src, &paths, &[])?;
            print!("{}", e.skeleton(&model).map_err(|e| e.0)?);
            Ok(())
        }
        Cmd::ValidateData { paths, model, src } => {
            let (e, rest) = load(&src, &paths, &["DATA"])?;
            let data = &rest[0];
            let errs = e.validate_data(&model, &read_data(data)?).map_err(|e| e.0)?;
            if errs.is_empty() {
                println!("ok: {} is valid {model} data", data.display());
                return Ok(());
            }
            for err in &errs { eprintln!("error: {err}"); }
            Err(format!("{}: {} error(s)", data.display(), errs.len()))
        }
        Cmd::Fmt { paths, keep_order, check } => {
            let mut changed = Vec::new();
            for p in template_files(&paths)? {
                let text = read_text(&p)?;
                netcfg::model::parse(&p.display().to_string(), &text).map_err(|e| e.0)?;
                let out = netcfg::fmt::format(&text, !keep_order);
                if out != text {
                    if !check { std::fs::write(&p, &out).map_err(|e| format!("{}: {e}", p.display()))?; }
                    changed.push(p.display().to_string());
                }
            }
            for c in &changed { println!("{} {c}", if check { "would reformat" } else { "reformatted" }); }
            if check && !changed.is_empty() { return Err(format!("{} file(s) need formatting", changed.len())); }
            Ok(())
        }
        Cmd::Bench { paths, model, runs, src } => {
            let (e, rest) = load(&src, &paths, &["CONFIG"])?;
            let text = read_text(&rest[0])?;
            let lines = text.lines().count();
            let mut lex_ms = Vec::new();
            let mut parse_ms = Vec::new();
            let mut render_ms = Vec::new();
            let mut rendered = 0;
            for _ in 0..runs {
                let t0 = std::time::Instant::now();
                let nodes = e.dialect.lex(&text);
                let t1 = std::time::Instant::now();
                let p = e.parse_nodes(&model, &nodes).map_err(|e| e.0)?;
                let t2 = std::time::Instant::now();
                let out = e.render(&model, &p.value).map_err(|e| e.0)?;
                let t3 = std::time::Instant::now();
                rendered = out.len();
                lex_ms.push((t1 - t0).as_secs_f64() * 1e3);
                parse_ms.push((t2 - t1).as_secs_f64() * 1e3);
                render_ms.push((t3 - t2).as_secs_f64() * 1e3);
            }
            let med = |v: &mut Vec<f64>| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] };
            let (l, m, r) = (med(&mut lex_ms), med(&mut parse_ms), med(&mut render_ms));
            println!("{lines} lines, {:.1} MB; median of {runs}: lex {l:.1} ms, match {m:.1} ms, parse {:.1} ms ({:.0} k lines/s), render {r:.1} ms ({:.1} MB out)",
                text.len() as f64 / 1e6, l + m, lines as f64 / (l + m), rendered as f64 / 1e6);
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("error: {e}"); ExitCode::from(1) }
    }
}
