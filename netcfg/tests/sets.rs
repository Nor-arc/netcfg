//! Phase 4: set manifests.

use netcfg::Engine;
use std::path::{Path, PathBuf};
use std::process::Command;

fn templates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates")
}

/// A fresh scratch tree under the target dir, holding the given files (paths may nest).
fn scratch(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (f, text) in files {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn netcfg(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_netcfg")).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

const HOST: &str = "model Host\n  hostname: string\n  routes: [Route]\n\ntemplate Host\n  hostname {{ hostname }}\n  << routes >>\n";
const ROUTE: &str = "model Route\n  prefix: key cidr\n\ntemplate Route\n  ip route {{ prefix }}\n";

#[test]
fn example_sets_load_from_manifests() {
    let e = Engine::load_set(&templates().join("nxos/set.nct")).unwrap();
    let m = e.set.as_ref().unwrap();
    assert_eq!((m.name.as_str(), m.root.as_deref()), ("nxos", Some("Device")));
    assert_eq!(e.templates_version(), "2026.09.1");
    // The shared route-map file comes in through `include`.
    assert!(e.model("RouteMapEntry").is_some());
    let p = e.parse("Device", "hostname h\n").unwrap();
    assert_eq!(p.templates_version, "2026.09.1");
    assert_eq!(p.engine_version, env!("CARGO_PKG_VERSION"));
    // A directory with a manifest in it is that set.
    assert_eq!(Engine::load_dir(&templates().join("eos"), None).unwrap().templates_version(), "2026.09.1");

    let all = Engine::load_all(&templates()).unwrap();
    assert_eq!(all.keys().collect::<Vec<_>>(), vec!["eos", "ios", "junos", "nxos"]);
    assert!(all.values().all(|e| e.templates_version() == "2026.09.1"));
}

#[test]
fn directory_loads_are_unversioned() {
    let dir = scratch("set_plain", &[("host.nct", HOST), ("route.nct", ROUTE)]);
    let e = Engine::load_dir(&dir, Some("nxos")).unwrap();
    assert!(e.set.is_none());
    assert_eq!(e.parse("Host", "hostname h\n").unwrap().templates_version, "unversioned");
}

#[test]
fn manifest_files_and_includes() {
    let dir = scratch("set_layout", &[
        ("common/route.nct", ROUTE),
        ("common/unused.nct", "model Unused\n  x: int?\n\ntemplate Unused\n  x {{ x }}\n"),
        ("dev/set.nct", "# Lab devices.\nset lab\n  dialect: nxos\n  version: 1.2.3   # bumped per release\n  include: ../common/route.nct\n  files: host*.nct, extra/**/*.nct\n  root: Host\n"),
        ("dev/host.nct", HOST),
        ("dev/other.nct", "model Other\n  x: int?\n\ntemplate Other\n  x {{ x }}\n"),
        ("dev/extra/deep/ntp.nct", "model Ntp\n  server: ipv4\n\ntemplate Ntp\n  ntp server {{ server }}\n"),
        ("dev/host.test.nct", "test \"t\"\n  model: Host\n"),
    ]);
    let e = Engine::load_set(&dir.join("dev/set.nct")).unwrap();
    assert_eq!(e.model_names(), vec!["Host", "Ntp", "Route"]);
    assert_eq!(e.templates_version(), "1.2.3");
    assert_eq!(e.dialect.name, "nxos");
    let p = e.parse("Host", "hostname h\nip route 10.0.0.0/8\n").unwrap();
    assert_eq!(p.value.to_json()["routes"][0]["prefix"], "10.0.0.0/8");
    // The directory form picks up the manifest too.
    assert_eq!(Engine::load_dir(&dir.join("dev"), None).unwrap().model_names(), vec!["Host", "Ntp", "Route"]);
}

#[test]
fn manifest_errors() {
    let with = |set: &str, extra: &[(&str, &str)]| {
        let mut files = vec![("s/set.nct", set), ("s/host.nct", HOST), ("s/route.nct", ROUTE)];
        files.extend_from_slice(extra);
        let dir = scratch("set_errors", &files);
        Engine::load_set(&dir.join("s/set.nct")).map(|_| ()).unwrap_err().0
    };
    let err = with("set s\n  dialect: nxos\n  include: ../nope/*.nct\n", &[]);
    assert!(err.contains("set s: include `../nope/*.nct` matches no file"), "{err}");
    let err = with("set s\n  dialect: nxos\n  colour: blue\n", &[]);
    assert!(err.contains("set.nct:3: unknown set property `colour` (known: dialect, version, include, files, root)"), "{err}");
    let err = with("set s\n  dialect: nxos\n  version: 1\n  version: 2\n", &[]);
    assert!(err.contains("set.nct:4: `version` is given twice"), "{err}");
    let err = with("set s\n  dialect: nxos\n  root: Nope\n", &[]);
    assert!(err.contains("set s: root `Nope` is not a model (known: Host, Route)"), "{err}");
    let err = with("set s\n  dialect: eos\n", &[("s/dialect.nct", "dialect nxos\n  extends: cisco\n")]);
    assert!(err.contains("set s names dialect `eos`, but its files declare `dialect nxos`"), "{err}");
    let err = with("set s\n  dialect: nxos\n  include: ../other/route.nct\n", &[("other/route.nct", ROUTE)]);
    assert!(err.contains("model `Route` is already defined at"), "{err}");
    let dir = scratch("set_not", &[("host.nct", HOST)]);
    assert!(Engine::load_set(&dir.join("host.nct")).unwrap_err().0.contains("not a set manifest (no `set NAME` section)"));
    let dir = scratch("set_twice", &[("a/set.nct", "set x\n  dialect: nxos\n"), ("a/h.nct", HOST), ("a/r.nct", ROUTE), ("b/set.nct", "set x\n  dialect: nxos\n"), ("b/h.nct", HOST), ("b/r.nct", ROUTE)]);
    assert!(Engine::load_all(&dir).unwrap_err().0.contains("set `x` is defined twice"));
    let dir = scratch("set_two_here", &[("a.nct", "set a\n  dialect: nxos\n"), ("b.nct", "set b\n  dialect: nxos\n")]);
    assert!(Engine::load_dir(&dir, None).unwrap_err().0.contains("several set manifests"));
}

#[test]
fn cli_accepts_set_or_directory() {
    let dir = scratch("set_cli", &[("cfg.txt", "hostname h\nrouter bgp 1\n  neighbor 10.0.0.1\n    remote-as 2\n")]);
    let cfg = dir.join("cfg.txt");
    let (cfg, set) = (cfg.to_str().unwrap(), templates().join("nxos/set.nct"));
    let set = set.to_str().unwrap();
    let a = netcfg(&["parse", "--set", set, cfg, "--model", "Device", "--format", "json"]);
    let b = netcfg(&["parse", templates().join("nxos").to_str().unwrap(), cfg, "--model", "Device", "--format", "json"]);
    let c = netcfg(&["parse", set, cfg, "--model", "Device", "--format", "json"]);
    assert!(a.0 && b.0 && c.0, "{a:?} {b:?} {c:?}");
    assert_eq!(a.1, b.1);
    assert_eq!(a.1, c.1);
    let (ok, out, _) = netcfg(&["validate", templates().to_str().unwrap()]);
    assert!(ok && out.lines().count() == 4 && out.contains("ok: set junos 2026.09.1"), "{out}");
    let (ok, _, err) = netcfg(&["parse", "--set", set, "--model", "Device", templates().to_str().unwrap(), cfg]);
    assert!(!ok && err.contains("expected CONFIG argument(s), got 2"), "{err}");
    for cmd in [vec!["explain", "--set", set, "--model", "Neighbor"], vec!["schema", "--set", set, "--model", "Device"], vec!["skeleton", "--set", set, "--model", "Device"], vec!["validate", "--set", set]] {
        let (ok, _, err) = netcfg(&cmd);
        assert!(ok, "{cmd:?}: {err}");
    }
}
