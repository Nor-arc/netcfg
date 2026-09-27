//! Phase 1 of the format: `.nct` files, named templates, removed scalar types, `<< >>`
//! markers and singleton nested models.

use netcfg::{Engine, Value};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh scratch directory under the target dir, holding the given files.
fn scratch(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
    dir
}

fn netcfg(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_netcfg")).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn load_err(text: &str) -> String {
    Engine::from_text("t.nct", text, Some("nxos")).unwrap_err().0
}

// ---- 1.1 file extension ----------------------------------------------------------------------

#[test]
fn nct_loads_and_ttp_is_deprecated() {
    let host = "model Host\n  hostname: string\n\ntemplate Host\n  hostname {{ hostname }}\n";
    let dir = scratch("ext_nct", &[("host.nct", host)]);
    let e = Engine::load_dir(&dir, Some("nxos")).unwrap();
    assert_eq!(e.model_names(), vec!["Host"]);
    assert!(e.warnings.is_empty());

    let dir = scratch("ext_ttp", &[("host.ttp", host)]);
    let e = Engine::load_dir(&dir, Some("nxos")).unwrap();
    assert_eq!(e.model_names(), vec!["Host"]);
    assert!(e.warnings[0].contains("host.ttp: the `.ttp` extension is deprecated; rename the file to `.nct`"), "{:?}", e.warnings);
    let (ok, _, err) = netcfg(&["validate", dir.to_str().unwrap(), "--dialect", "nxos"]);
    assert!(ok);
    assert!(err.contains("warning: ") && err.contains("host.ttp: the `.ttp` extension is deprecated"), "{err}");
}

#[test]
fn test_files_are_not_templates() {
    let dir = scratch("ext_test", &[
        ("host.nct", "model Host\n  hostname: string\n\ntemplate Host\n  hostname {{ hostname }}\n"),
        ("host.test.nct", "test \"not a template\"\n  model: Host\n"),
    ]);
    assert_eq!(Engine::load_dir(&dir, Some("nxos")).unwrap().model_names(), vec!["Host"]);
}

// ---- 1.2 named templates ---------------------------------------------------------------------

#[test]
fn templates_pair_by_name_across_files() {
    let dir = scratch("named_split", &[
        ("a_models.nct", "model Nbr\n  peer: key ip\n  remoteAs: asn?\n\nmodel Bgp\n  asn: key asn\n  nbrs: [Nbr]\n"),
        ("b_templates.nct", "template Bgp\n  router bgp {{ asn }}\n    << nbrs >>\n\ntemplate Nbr\n  neighbor {{ peer }}\n    remote-as {{ remoteAs }}\n"),
    ]);
    let e = Engine::load_dir(&dir, Some("nxos")).unwrap();
    let cfg = "router bgp 1\n  neighbor 10.0.0.1\n    remote-as 2\n";
    let p = e.parse("Bgp", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"asn": 1, "nbrs": [{"peer": "10.0.0.1", "remoteAs": 2}]}));
    assert_eq!(e.render("Bgp", &p.value).unwrap(), cfg);
}

#[test]
fn template_pairing_errors() {
    let err = load_err("model A\n  x: int\n\nmodel B\n  y: int\n\ntemplate A\n  a {{ x }}\n");
    assert!(err.contains("t.nct:4: model B has no template; add a `template B` section"), "{err}");
    let err = load_err("model A\n  x: int\n\ntemplate A\n  a {{ x }}\n\ntemplate C\n  c {{ x }}\n");
    assert!(err.contains("t.nct:7: `template C` names no model or fragment (known: A)"), "{err}");
    let err = load_err("model A\n  x: int\n\ntemplate A\n  a {{ x }}\n\ntemplate A\n  b {{ x }}\n");
    assert!(err.contains("t.nct:7: second template for A (the first is at t.nct:4)"), "{err}");
    // Template line numbers are the file's, wherever the template sits.
    let err = load_err("template A\n  a {{ y }}\n\nmodel A\n  x: int\n");
    assert!(err.contains("template line 2 `a {{ y }}`: `y` is not a field of A"), "{err}");
}

#[test]
fn bare_template_warns_and_fmt_rewrites() {
    let old = "# Hosts.\nmodel Host\n  hostname: string\n\ntemplate\n  hostname {{ hostname }}\n";
    let e = Engine::from_text("old.nct", old, Some("nxos")).unwrap();
    assert_eq!(e.warnings, vec!["old.nct:5: bare `template` is deprecated; write `template Host` (`netcfg fmt` rewrites files)"]);

    let dir = scratch("fmt", &[("old.nct", old)]);
    let d = dir.to_str().unwrap();
    let (ok, _, err) = netcfg(&["fmt", "--check", d]);
    assert!(!ok && err.contains("1 file(s) need formatting"), "{err}");
    let (ok, out, _) = netcfg(&["fmt", d]);
    assert!(ok && out.contains("reformatted"), "{out}");
    assert_eq!(std::fs::read_to_string(dir.join("old.nct")).unwrap(), old.replace("template\n", "template Host\n"));
    assert!(netcfg(&["fmt", "--check", d]).0);
    assert!(Engine::load_dir(&dir, Some("nxos")).unwrap().warnings.is_empty());
}

// ---- 1.3 removed scalar types ----------------------------------------------------------------

#[test]
fn removed_types_suggest_their_replacement() {
    for (ty, hint) in [("names", "use list(string)"), ("ints", "use list(int)"), ("intpair", "{{ keepalive: int }} {{ hold: int }}")] {
        let err = load_err(&format!("model A\n  x: {ty}?\n\ntemplate A\n  a {{{{ x }}}}\n"));
        assert!(err.contains(&format!("type `{ty}` was removed; {}", if ty == "intpair" { "use a struct" } else { hint })) && err.contains(hint), "{err}");
    }
    let err = load_err("type t = {{ a: names }}\nmodel A\n  x: t?\n\ntemplate A\n  a {{ x }}\n");
    assert!(err.contains("type `names` was removed; use list(string)"), "{err}");
}

#[test]
fn list_int_is_strict() {
    let e = Engine::from_text("t", "model A\n  vlans: list(int)?\n\ntemplate A\n  allowed vlan {{ vlans }}\n", Some("nxos")).unwrap();
    assert_eq!(e.parse("A", "allowed vlan 10 20\n").unwrap().value.to_json(), json!({"vlans": [10, 20]}));
    let err = e.parse("A", "allowed vlan 10 twenty\n").unwrap_err();
    assert!(err.0.contains("'twenty' is not an integer"), "{err}");
}

#[test]
fn example_timers_are_a_struct() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/eos");
    let e = Engine::load_dir(&dir, None).unwrap();
    let cfg = "hostname h\nrouter bgp 1\n   neighbor 10.0.0.1 remote-as 2\n   neighbor 10.0.0.1 timers 10 30\n";
    let p = e.parse("EosDevice", cfg).unwrap();
    assert_eq!(p.value.to_json()["bgp"][0]["neighbors"][0]["timers"], json!({"keepalive": 10, "hold": 30}));
    assert_eq!(e.render("EosDevice", &p.value).unwrap(), cfg);
}

// ---- 1.4 << >> markers -----------------------------------------------------------------------

#[test]
fn nested_marker_rules() {
    let base = "model N\n  peer: key ip\n\ntemplate N\n  neighbor {{ peer }}\n\nmodel B\n  asn: key asn\n  hostname: string?\n  on: flag\n  nbrs: [N]\n  one: N?\n\n";
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    {{{{ nbrs }}}}\n    << one >>\n"));
    assert!(err.contains("`nbrs` is a nested model: write it as << nbrs >> alone on its line, not {{ nbrs }}"), "{err}");
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    [[ nbrs ]]\n    << one >>\n"));
    assert!(err.contains("`nbrs` is a nested model: write it as << nbrs >>"), "{err}");
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    hostname << hostname >>\n    x << on >>\n    << nbrs >>\n    << one >>\n"));
    assert!(err.contains("`hostname` is a value, not a nested model: << >> is for [Model] and Model? fields; a value is written {{ hostname }}"), "{err}");
    assert!(err.contains("`on` is a flag, not a nested model: write it as [[ on ]]"), "{err}");
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    peers << nbrs >>\n    << one >>\n"));
    assert!(err.contains("<< nbrs >> must be alone on its line"), "{err}");
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    << nbrz >>\n    << nbrs >>\n    << one >>\n"));
    assert!(err.contains("`nbrz` is not a field of B"), "{err}");
    let err = load_err(&format!("{base}template B\n  router bgp {{{{ asn }}}}\n    <<nbrs\n    << one >>\n"));
    assert!(err.contains("placeholder in `<<nbrs` must be written"), "{err}");
}

#[test]
fn explain_shows_nested_rows() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/nxos");
    let x = Engine::load_dir(&dir, None).unwrap().explain("Bgp").unwrap();
    assert!(x.contains("neighbors (list of Neighbor)\n  each     → one Neighbor block/group\n"), "{x}");
}

// ---- 1.5 singleton nested models -------------------------------------------------------------

const SINGLE: &str = "model Ospf
  pid: key int
  routerId: ipv4?

template Ospf
  router ospf {{ pid }}
    router-id {{ routerId }}

model Snmp
  location: phrase?
  contact: phrase?

template Snmp
  snmp-server
    location {{ location }}
    contact {{ contact }}

model Ntp
  server: ipv4

template Ntp
  ntp server {{ server }}

model Device
  hostname: string
  ospf: Ospf?
  snmp: Snmp?
  ntp: Ntp

template Device
  hostname {{ hostname }}
  << ospf >>
  << snmp >>
  << ntp >>
";

#[test]
fn singleton_parse_render_round_trip() {
    let e = Engine::from_text("t", SINGLE, Some("nxos")).unwrap();
    let cfg = "hostname r1\nrouter ospf 1\n  router-id 1.1.1.1\nsnmp-server\n  location dc1 row 4\nntp server 10.0.0.1\n";
    let p = e.parse("Device", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"hostname": "r1", "ospf": {"pid": 1, "routerId": "1.1.1.1"}, "snmp": {"location": "dc1 row 4"}, "ntp": {"server": "10.0.0.1"}}));
    assert!(p.unmanaged.is_empty());
    assert_eq!(e.render("Device", &p.value).unwrap(), cfg);
    // Absent optional singletons: the key is missing.
    let p = e.parse("Device", "hostname r1\nntp server 10.0.0.1\n").unwrap();
    assert_eq!(p.value.to_json(), json!({"hostname": "r1", "ntp": {"server": "10.0.0.1"}}));
    assert_eq!(e.render("Device", &p.value).unwrap(), "hostname r1\nntp server 10.0.0.1\n");
}

#[test]
fn singleton_duplicate_and_required_errors() {
    let e = Engine::from_text("t", SINGLE, Some("nxos")).unwrap();
    let base = "hostname r1\nntp server 10.0.0.1\n";
    let err = e.parse("Device", &format!("{base}router ospf 1\nrouter ospf 2\n")).unwrap_err();
    assert!(err.0.contains("duplicate Ospf: `router ospf 2` (a single Ospf is allowed here)"), "{err}");
    let err = e.parse("Device", &format!("{base}snmp-server\n  location a\nsnmp-server\n  contact b\n")).unwrap_err();
    assert!(err.0.contains("duplicate Snmp: `snmp-server`"), "{err}");
    let err = e.parse("Device", &format!("{base}ntp server 10.0.0.2\n")).unwrap_err();
    assert!(err.0.contains("duplicate Ntp: `ntp server 10.0.0.2`"), "{err}");
    let err = e.parse("Device", "hostname r1\n").unwrap_err();
    assert!(err.0.contains("required Ntp (`ntp`) is missing"), "{err}");
    // Rendering checks the same.
    let err = e.render("Device", &Value::from_json(&json!({"hostname": "r1"}))).unwrap_err();
    assert!(err.0.contains("field `ntp` is missing (a required Ntp)"), "{err}");
    let err = e.render("Device", &Value::from_json(&json!({"hostname": "r1", "ntp": {"server": "10.0.0.1"}, "ospf": [{"pid": 1}]}))).unwrap_err();
    assert!(err.0.contains("field `ospf`: expected a record (Ospf)"), "{err}");
    // Strictness inside the singleton still applies.
    let err = e.parse("Device", &format!("{base}router ospf 1\n  router-id nope\n")).unwrap_err();
    assert!(err.0.contains("'nope' is not an IPv4 address"), "{err}");
}

#[test]
fn flat_singleton() {
    let t = "model Lp\n  peer: key ip\n  remoteAs: asn?\n  desc: phrase?\n\ntemplate Lp\n  neighbor {{ peer }} remote-as {{ remoteAs }}\n  neighbor {{ peer }} description {{ desc }}\n\nmodel B\n  asn: key asn\n  lp: Lp?\n\ntemplate B\n  router bgp {{ asn }}\n    << lp >>\n";
    let e = Engine::from_text("t", t, Some("eos")).unwrap();
    let cfg = "router bgp 1\n   neighbor 10.0.0.1 remote-as 2\n   neighbor 10.0.0.1 description x\n";
    let p = e.parse("B", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"asn": 1, "lp": {"peer": "10.0.0.1", "remoteAs": 2, "desc": "x"}}));
    assert_eq!(e.render("B", &p.value).unwrap(), cfg);
    let err = e.parse("B", &format!("{cfg}   neighbor 10.0.0.2 remote-as 3\n")).unwrap_err();
    assert!(err.0.contains("duplicate Lp: `neighbor 10.0.0.2 remote-as 3`"), "{err}");
}

#[test]
fn unkeyed_singleton_needs_one_top_level_line() {
    let t = "model S\n  a: int?\n  b: int?\n\ntemplate S\n  a {{ a }}\n  b {{ b }}\n\nmodel D\n  s: S?\n\ntemplate D\n  << s >>\n";
    let err = load_err(t);
    assert!(err.contains("field `s`: S has no key, so as a singleton it is identified by its header line; its template must have exactly one top-level line"), "{err}");
}

#[test]
fn singleton_explain_and_schema() {
    let e = Engine::from_text("t", SINGLE, Some("nxos")).unwrap();
    let x = e.explain("Device").unwrap();
    assert!(x.contains("ospf (single Ospf, optional)\n  value    → one Ospf block/group (a second is an error)\n  missing  → (nothing written)\n"), "{x}");
    assert!(x.contains("ntp (single Ntp, required)\n"), "{x}");
    let s = e.schema("Device").unwrap();
    assert_eq!(s["$defs"]["Device"]["properties"]["ospf"], json!({"$ref": "#/$defs/Ospf"}));
    assert_eq!(s["$defs"]["Device"]["required"], json!(["hostname", "ntp"]));
    assert_eq!(s["$defs"]["Ospf"]["type"], "object");
    assert!(s["$defs"]["Snmp"].is_object() && s["$defs"]["Ntp"].is_object());
}
