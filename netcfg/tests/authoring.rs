//! Phase 6: template tests, golden checks, suggestions, lint and cross-field references.

use netcfg::lint::Goldens;
use netcfg::{testfile, Engine, Value};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn templates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates")
}
fn scratch(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (f, text) in files {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn netcfg(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_netcfg")).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}
fn data(j: serde_json::Value) -> Value {
    Value::from_json(&j)
}

// ---- 6.1 netcfg test -------------------------------------------------------------------------

#[test]
fn example_sets_pass_their_tests() {
    for set in ["eos", "nxos", "ios", "junos"] {
        let e = Engine::load_dir(&templates().join(set), None).unwrap();
        let results = e.run_tests_in(&e.tests_dir().unwrap()).unwrap();
        assert!(results.len() >= 2, "{set}: {} tests", results.len());
        for (t, r) in &results {
            assert!(r.is_ok(), "{set}: {}:{} {}: {}", t.source, t.line, t.name, r.as_ref().unwrap_err());
        }
    }
}

const TESTS: &str = r#"# Comments between tests are fine.
test "passes"
  model: M
  config:
    a 1
    b hello world
  expect:
    a: 1
    b: hello world
  roundtrip: true
  render: |
    a 1
    b hello world

test "wrong expectation"
  model: M
  config: a 1
  expect: {a: 2}

test "expected failure that parses"
  model: M
  config: a 1
  fails: not an integer

test "extra key in expect"
  model: M
  config: a 1
  expect: {a: 1, b: x}

test "unmanaged mismatch"
  model: M
  config:
    a 1
    c 3
  unmanaged: []

test "flags at default may be left out, or given"
  model: M
  config: a 1
  expect: {a: 1, on: false}
"#;

#[test]
fn test_file_format_and_outcomes() {
    let e = Engine::from_text("t", "model M\n  a: int?\n  b: phrase?\n  on: flag\n\ntemplate M\n  a {{ a }}\n  b {{ b }}\n  on [[ on ]]\n", Some("nxos")).unwrap();
    let cases = testfile::parse("t.test.nct", TESTS).unwrap();
    assert_eq!(cases.len(), 6);
    assert_eq!((cases[0].line, cases[0].config.as_str()), (2, "a 1\nb hello world\n"));
    let outcomes: Vec<Result<(), String>> = cases.iter().map(|t| e.run_test(t)).collect();
    assert!(outcomes[0].is_ok(), "{:?}", outcomes[0]);
    assert!(outcomes[1].as_ref().unwrap_err().contains("parsed data differs\n  expected: {\"a\":2}\n  got:      {\"a\":1}"), "{:?}", outcomes[1]);
    assert!(outcomes[2].as_ref().unwrap_err().contains("expected an error containing `not an integer`, but it parsed"), "{:?}", outcomes[2]);
    assert!(outcomes[3].is_err(), "missing keys must be missing");
    assert!(outcomes[4].as_ref().unwrap_err().contains("unmanaged lines differ"), "{:?}", outcomes[4]);
    assert!(outcomes[5].is_ok(), "{:?}", outcomes[5]);

    for (bad, msg) in [
        ("test \"x\"\n  model: M\n  config: a 1\n  colour: red\n", "t.test.nct:4: test \"x\": unknown property `colour` (known: model, config, expect, unmanaged, roundtrip, render, fails)"),
        ("test \"x\"\n  config: a 1\n", "t.test.nct:1: test \"x\" needs `model:`"),
        ("test \"x\"\n  model: M\n", "needs `config:`"),
        ("test \"x\"\n  model: M\n  config: a 1\n  fails: y\n  roundtrip: true\n", "`fails` cannot be combined"),
        ("test \"x\"\n  model: M\n  config: a 1\n  roundtrip: maybe\n", "`roundtrip` must be true or false"),
        ("test \"x\"\n  model: M\n  config: a 1\n  expect: {a: [\n", "`expect` is not valid YAML"),
        ("tset \"x\"\n", "t.test.nct:1: expected `test \"name\"`"),
    ] {
        let err = testfile::parse("t.test.nct", bad).unwrap_err().0;
        assert!(err.contains(msg), "{err}");
    }
}

#[test]
fn test_cli_reports_and_fails() {
    let dir = scratch("test_cli", &[
        ("set.nct", "set t\n  dialect: nxos\n"),
        ("m.nct", "model M\n  a: int?\n\ntemplate M\n  a {{ a }}\n"),
        ("m.test.nct", "test \"good\"\n  model: M\n  config: a 1\n  expect: {a: 1}\n\ntest \"bad\"\n  model: M\n  config: a 1\n  expect: {a: 2}\n"),
    ]);
    let (ok, out, err) = netcfg(&["test", "--set", dir.join("set.nct").to_str().unwrap()]);
    assert!(!ok && err.contains("1 test(s) failed"), "{err}");
    assert!(out.contains("ok    ") && out.contains(" good\n") && out.contains("FAIL  ") && out.contains("1 passed, 1 failed"), "{out}");
    let (ok, out, _) = netcfg(&["test", templates().join("eos").to_str().unwrap()]);
    assert!(ok && out.contains("9 passed, 0 failed"), "{out}");
}

#[test]
fn canonical_data_drops_defaults_only() {
    let e = Engine::load_dir(&templates().join("ios"), None).unwrap();
    let v = e.parse("Interface", "interface Gi0/1\n mtu 1500\n no description\n").unwrap().value;
    assert_eq!(v.to_json(), json!({"name": "Gi0/1", "description": null, "mtu": 1500, "shutdown": false}));
    assert_eq!(e.canonical_data("Interface", &v).unwrap().to_json(), json!({"name": "Gi0/1", "description": null}));
}

// ---- 6.2 netcfg check ------------------------------------------------------------------------

#[test]
fn example_goldens_match_their_committed_reports() {
    let e = Engine::load_set(&templates().join("nxos/set.nct")).unwrap();
    let results = e.check_goldens(&templates().join("nxos/golden"), e.root_model(None).unwrap(), true).unwrap();
    assert!(!results.is_empty());
    for r in results { assert_eq!(r.problem, None, "{}", r.config.display()); }
}

#[test]
fn check_writes_then_compares() {
    let set = templates().join("nxos/set.nct");
    let dir = scratch("check_goldens", &[
        ("a.cfg", "hostname a\nfeature bgp\n"),
        ("site/b.cfg", "hostname b\nrouter bgp 1\n  neighbor 10.0.0.1\n    bfd\n"),
        (".hidden/c.cfg", "garbage that is never read\n"),
    ]);
    let (s, d) = (set.to_str().unwrap(), dir.to_str().unwrap());
    let (ok, out, err) = netcfg(&["check", "--set", s, "--golden", d]);
    assert!(ok, "{err}");
    assert!(out.contains("ok    a.cfg (1 unmanaged)") && out.contains("site/b.cfg (1 unmanaged)") && out.contains("2 config(s), 0 failed"), "{out}");
    assert_eq!(std::fs::read_to_string(dir.join(".unmanaged/site/b.cfg.txt")).unwrap(), "router bgp 1 > neighbor 10.0.0.1 > bfd\n");
    assert!(netcfg(&["check", "--set", s, "--golden", d, "--compare"]).0);
    // A template change (here: a config change) that alters the report fails the gate.
    std::fs::write(dir.join("a.cfg"), "hostname a\nfeature bgp\nfeature lldp\n").unwrap();
    let (ok, out, _) = netcfg(&["check", "--set", s, "--golden", d, "--compare"]);
    assert!(!ok && out.contains("FAIL  a.cfg") && out.contains("+ feature lldp"), "{out}");
    // Strict errors fail, with the message.
    std::fs::write(dir.join("a.cfg"), "hostname a\nrouter bgp 1\n  neighbor 10.0.0.1\n    ebgp-multihop 999\n").unwrap();
    let (ok, out, _) = netcfg(&["check", "--set", s, "--golden", d]);
    assert!(!ok && out.contains("FAIL  a.cfg") && out.contains("outside 2..255"), "{out}");
    // No report committed yet.
    std::fs::write(dir.join("new.cfg"), "hostname n\n").unwrap();
    let (ok, out, _) = netcfg(&["check", "--set", s, "--golden", d, "--compare"]);
    assert!(!ok && out.contains("no committed report"), "{out}");
    // A set without `root` needs --model.
    let plain = scratch("check_noroot", &[("m.nct", "model M\n  a: int?\n\ntemplate M\n  a {{ a }}\n")]);
    let (ok, _, err) = netcfg(&["check", plain.to_str().unwrap(), "--dialect", "nxos", "--golden", d]);
    assert!(!ok && err.contains("give --model, or declare `root:`"), "{err}");
}

// ---- 6.3 netcfg suggest ----------------------------------------------------------------------

#[test]
fn suggestions_group_by_literal_prefix() {
    let e = Engine::load_dir(&templates().join("eos"), None).unwrap();
    let cfg = "hostname h
router bgp 1
   neighbor 10.0.0.1 remote-as 2
   neighbor 10.0.0.1 bfd
   neighbor 10.0.0.1 password 7 abc
   neighbor 10.0.0.1 local-as 65001
   neighbor 10.0.0.2 remote-as 3
   neighbor 10.0.0.2 bfd
   neighbor 10.0.0.2 password 7 def
   neighbor 10.0.0.2 local-as 65002
   maximum-paths 4
ip name-server 10.9.9.9
ip name-server 10.9.9.8
banner motd Welcome to r1
";
    let s = e.suggest("EosDevice", cfg).unwrap();
    let rows: Vec<(String, usize, String, String, bool)> = s.iter().map(|x| (x.path.clone(), x.count, x.template_line.clone(), x.field.clone(), x.ignored)).collect();
    let want = |p: &str, n: usize, l: &str, f: &str, ig: bool| (p.to_string(), n, l.to_string(), f.to_string(), ig);
    assert!(rows.contains(&want("router bgp 1 > neighbor * bfd", 2, "neighbor {{ key }} bfd [[ neighborBfd ]]", "neighborBfd: flag", false)), "{rows:#?}");
    assert!(rows.contains(&want("router bgp 1 > neighbor * password 7 *", 2, "neighbor {{ key }} password 7 {{ neighborPassword }}", "neighborPassword: string?", true)), "{rows:#?}");
    assert!(rows.contains(&want("router bgp 1 > neighbor * local-as *", 2, "neighbor {{ key }} local-as {{ neighborLocalAs }}", "neighborLocalAs: int?", false)), "{rows:#?}");
    assert!(rows.contains(&want("ip name-server *", 2, "ip name-server {{ ipNameServer }}", "ipNameServer: ipv4?", false)), "{rows:#?}");
    assert!(rows.contains(&want("banner motd *", 1, "banner motd {{ bannerMotd }}", "bannerMotd: phrase?", false)), "{rows:#?}");
    assert!(rows.contains(&want("router bgp 1 > maximum-paths *", 1, "maximum-paths {{ maximumPaths }}", "maximumPaths: int?", false)), "{rows:#?}");
    // Most frequent first; the text form is aligned.
    assert!(s[0].count >= s[s.len() - 1].count);
    let dir = scratch("suggest_cli", &[("c.cfg", cfg)]);
    let (ok, out, _) = netcfg(&["suggest", templates().join("eos").to_str().unwrap(), dir.join("c.cfg").to_str().unwrap(), "--model", "EosDevice"]);
    assert!(ok && out.contains("(2x)  neighbor {{ key }} password 7 {{ neighborPassword }}") && out.contains("(covered by @ignore)"), "{out}");
}

// ---- 6.4 netcfg lint -------------------------------------------------------------------------

#[test]
fn lint_finds_shadowed_lines_and_ambiguous_claims() {
    let t = "model A\n  k: key string\n  x: int?\n\ntemplate A\n  alpha {{ k }}\n\nmodel B\n  k: key string\n\ntemplate B\n  alpha {{ k }}\n\nmodel D\n  hops: int?\n  hopsText: string?\n  mode: string?\n  on: flag\n  as: [A]\n  bs: [B]\n\ntemplate D\n  hops {{ hops }}\n  hops {{ hopsText }}\n  mode {{ mode }} strict\n  mode loose [[ on ]]\n  << as >>\n  << bs >>\n";
    let t = t.replace("  alpha {{ k }}\n\nmodel B", "  alpha {{ k }}\n    x {{ x }}\n\nmodel B");
    let e = Engine::from_text("t.nct", t.as_str(), Some("nxos")).unwrap();
    let w: Vec<String> = e.lint(None).iter().map(|w| w.to_string()).collect();
    assert!(w.iter().any(|w| w.contains("model D: template line 25: `hops {{ hopsText }}` is shadowed by `hops {{ hops }}` (template line 24)")), "{w:#?}");
    assert!(!w.iter().any(|w| w.contains("mode loose")), "a trailing literal distinguishes the lines: {w:#?}");
    assert!(w.iter().any(|w| w.contains("<< as >> (A) and << bs >> (B) both claim lines starting with `alpha`")), "{w:#?}");
    assert_eq!(w.len(), 2, "{w:#?}");
}

#[test]
fn lint_with_goldens() {
    let e = Engine::load_set(&templates().join("eos/set.nct")).unwrap();
    let g = Goldens { model: "EosDevice".into(), configs: vec![(PathBuf::from("g1.cfg"), "hostname h\nrouter bgp 1\n   neighbor 10.0.0.1 remote-as 2\n   neighbor 10.0.0.1 description x\n   neighbor 10.0.0.2 remote-as 3\n".into())] };
    let w: Vec<String> = e.lint(Some(&g)).iter().map(|w| w.to_string()).collect();
    assert!(w.iter().any(|w| w.contains("model RouteMapEntry never appears in the 1 golden config(s)")), "{w:#?}");
    assert!(w.iter().any(|w| w.contains("bgp.nct:12: model EosNeighbor: field `timers`: never set in the goldens (2 record(s))")), "{w:#?}");
    assert!(!w.iter().any(|w| w.contains("field `description`")), "{w:#?}");
    assert!(w.iter().any(|w| w.contains("field `shutdown`: flag is false (its declared default) in all 2 record(s)")), "{w:#?}");
    assert!(w.iter().any(|w| w.contains("`@ignore neighbor * password` matches no line in the goldens")), "{w:#?}");
    let bad = Goldens { model: "EosDevice".into(), configs: vec![(PathBuf::from("bad.cfg"), "router bgp 1\n   neighbor 10.0.0.1 remote-as x\n".into())] };
    assert!(e.lint(Some(&bad)).iter().any(|w| w.at == "bad.cfg" && w.message.contains("does not parse")));
    // CLI: warnings are not errors unless --strict.
    let (ok, out, _) = netcfg(&["lint", templates().join("nxos").to_str().unwrap(), "--golden", templates().join("nxos/golden").to_str().unwrap()]);
    assert!(ok && out.contains("warning: ") && out.contains("warning(s)"), "{out}");
    let (ok, _, err) = netcfg(&["lint", templates().join("nxos").to_str().unwrap(), "--golden", templates().join("nxos/golden").to_str().unwrap(), "--strict"]);
    assert!(!ok && err.contains("lint warning(s) (--strict)"), "{err}");
    assert!(netcfg(&["lint", templates().join("nxos").to_str().unwrap(), "--strict"]).0);
}

// ---- 6.5 cross-field references --------------------------------------------------------------

const REFS: &str = "model Rm
  name: key string
  seq: key int

template Rm
  route-map {{ name }} permit {{ seq }}

model Af
  afi: key string
  routeMapIn: string? references Rm.name   # must be a defined route-map

template Af
  address-family {{ afi }}
    route-map {{ routeMapIn }} in

model D
  rms: [Rm]
  afs: [Af]

template D
  << rms >>
  << afs >>
";

#[test]
fn references_are_checked_on_validate_render_and_diff() {
    let e = Engine::from_text("t.nct", REFS, Some("nxos")).unwrap();
    let good = data(json!({"rms": [{"name": "RM-IN", "seq": 10}, {"name": "RM-IN", "seq": 20}], "afs": [{"afi": "ipv4", "routeMapIn": "RM-IN"}, {"afi": "ipv6", "routeMapIn": null}]}));
    assert!(e.validate_data("D", &good).unwrap().is_empty());
    assert!(e.render("D", &good).is_ok());
    let bad = data(json!({"rms": [{"name": "RM-IN", "seq": 10}], "afs": [{"afi": "ipv4", "routeMapIn": "RM-OUT"}]}));
    let errs = e.validate_data("D", &bad).unwrap();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "D.afs[0]: field `routeMapIn`: \"RM-OUT\" is not a Rm.name in this data (known: RM-IN)");
    assert_eq!(e.render("D", &bad).unwrap_err().0, errs[0].0);
    let err = e.diff("D", &good, &bad).unwrap_err();
    assert!(err.0.contains("intent data is invalid") && err.0.contains("is not a Rm.name"), "{err}");
    let none = data(json!({"afs": [{"afi": "ipv4", "routeMapIn": "RM-OUT"}]}));
    assert!(e.validate_data("D", &none).unwrap()[0].0.contains("(known: none)"));
    // Data that cannot contain the referenced model is not checked.
    assert!(e.validate_data("Af", &data(json!({"afi": "ipv4", "routeMapIn": "RM-OUT"}))).unwrap().is_empty());
    // Parsing is not affected: config may reference undefined route-maps.
    assert!(e.parse("D", "address-family ipv4\n  route-map NOPE in\n").is_ok());
    // The schema ignores references; the doc still arrives.
    assert_eq!(e.schema("D").unwrap()["$defs"]["Af"]["properties"]["routeMapIn"]["anyOf"][0]["type"], "string");
}

#[test]
fn reference_declaration_errors() {
    let err = |from: &str, to: &str| Engine::from_text("t.nct", &REFS.replace(from, to), Some("nxos")).unwrap_err().0;
    assert!(err("references Rm.name", "references Nope.name").contains("field `routeMapIn`: references unknown model `Nope`"));
    assert!(err("references Rm.name", "references Rm.nam").contains("Rm has no field `nam`"));
    assert!(err("references Rm.name", "references Rm").contains("must name a model and its key field: `references Model.field`"));
    let e = err("  routeMapIn: string? references Rm.name", "  routeMapIn: flag references Rm.name").to_string();
    assert!(e.contains("only optional or required value fields can reference another model"), "{e}");
    let t = REFS.replace("  seq: key int\n", "  seq: key int\n  desc: string?\n").replace("route-map {{ name }} permit {{ seq }}\n", "route-map {{ name }} permit {{ seq }}\n    description {{ desc }}\n").replace("references Rm.name", "references Rm.desc");
    assert!(Engine::from_text("t.nct", &t, Some("nxos")).unwrap_err().0.contains("`Rm.desc` is not a key field of Rm"));
}
