use clap::{Parser, Subcommand, ValueEnum};
use netcfg::{Engine, Value};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "netcfg", version, about = "Bidirectional network config templates")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format { Json, Yaml }

#[derive(Subcommand)]
enum Cmd {
    /// Load every .ttp file under a directory and report all errors.
    Validate { templates: PathBuf, #[arg(long)] dialect: Option<String> },
    /// Parse a running config into model data (JSON/YAML) plus the unmanaged report.
    Parse {
        templates: PathBuf,
        config: PathBuf,
        #[arg(long)] model: String,
        /// Builtin dialect to use when the templates declare none (ios, nxos, eos, junos).
        #[arg(long)] dialect: Option<String>,
        #[arg(long, value_enum, default_value = "yaml")] format: Format,
        /// Print unmanaged lines (config the model doesn't cover) to stderr as paths.
        #[arg(long)] unmanaged: bool,
    },
    /// Render model data (JSON/YAML) to config text.
    Render { templates: PathBuf, data: PathBuf, #[arg(long)] model: String, #[arg(long)] dialect: Option<String> },
    /// Show how every field of a model is spelled in config (values, defaults, negations).
    Explain { templates: PathBuf, #[arg(long)] model: String, #[arg(long)] dialect: Option<String> },
    /// Print the JSON Schema for a model's data.
    Schema { templates: PathBuf, #[arg(long)] model: String, #[arg(long)] dialect: Option<String> },
    /// Time parse and render on a config.
    Bench { templates: PathBuf, config: PathBuf, #[arg(long)] model: String, #[arg(long)] dialect: Option<String>, #[arg(long, default_value_t = 5)] runs: usize },
}

fn load(templates: &PathBuf, dialect: &Option<String>) -> Result<Engine, String> {
    Engine::load_dir(templates, dialect.as_deref()).map_err(|e| e.0)
}

fn read_data(path: &PathBuf) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let j: serde_json::Value = if path.extension().map(|x| x == "json").unwrap_or(false) {
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    };
    Ok(Value::from_json(&j))
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Validate { templates, dialect: d } => {
            let e = load(&templates, &d)?;
            println!("ok: dialect {} ({:?} grammar), {} model(s): {}", e.dialect.name, e.dialect.grammar, e.model_names().len(), e.model_names().join(", "));
            Ok(())
        }
        Cmd::Parse { templates, config, model, dialect: d, format, unmanaged } => {
            let e = load(&templates, &d)?;
            let text = std::fs::read_to_string(&config).map_err(|e| format!("{}: {e}", config.display()))?;
            let p = e.parse(&model, &text).map_err(|e| e.0)?;
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
        Cmd::Render { templates, data, model, dialect: d } => {
            let e = load(&templates, &d)?;
            print!("{}", e.render(&model, &read_data(&data)?).map_err(|e| e.0)?);
            Ok(())
        }
        Cmd::Explain { templates, model, dialect: d } => {
            let e = load(&templates, &d)?;
            print!("{}", e.explain(&model).map_err(|e| e.0)?);
            Ok(())
        }
        Cmd::Schema { templates, model, dialect: d } => {
            let e = load(&templates, &d)?;
            println!("{}", serde_json::to_string_pretty(&e.schema(&model).map_err(|e| e.0)?).unwrap());
            Ok(())
        }
        Cmd::Bench { templates, config, model, dialect: d, runs } => {
            let e = load(&templates, &d)?;
            let text = std::fs::read_to_string(&config).map_err(|e| format!("{}: {e}", config.display()))?;
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
