//! Constant lines: template lines with no placeholders, which must be present, carry no data
//! and are always rendered.

use netcfg::{Engine, Value};
use serde_json::json;

fn eng(text: &str, dialect: &str) -> Engine {
    Engine::from_text("t.nct", text, Some(dialect)).unwrap()
}
fn data(j: serde_json::Value) -> Value {
    Value::from_json(&j)
}

const AF: &str = "model NeighborAf
  afi: key string
  safi: key string
  routeMapIn: string?

template NeighborAf
  address-family {{ afi }} {{ safi }}
    route-map {{ routeMapIn }} in
    exit-address-family

model Neighbor
  peer: key ip
  afs: [NeighborAf]

template Neighbor
  neighbor {{ peer }}
    << afs >>
";

const CFG: &str = "neighbor 10.0.0.1\n  address-family ipv4 unicast\n    route-map RM in\n    exit-address-family\n";

#[test]
fn block_body_constant_parses_renders_and_round_trips() {
    let e = eng(AF, "nxos");
    let p = e.parse("Neighbor", CFG).unwrap();
    assert_eq!(p.value.to_json(), json!({"peer": "10.0.0.1", "afs": [{"afi": "ipv4", "safi": "unicast", "routeMapIn": "RM"}]}));
    assert!(p.unmanaged.is_empty());
    assert_eq!(e.render("Neighbor", &p.value).unwrap(), CFG);
    // Rendered in template position whatever the config order, and regardless of data.
    let swapped = CFG.replace("    route-map RM in\n    exit-address-family\n", "    exit-address-family\n    route-map RM in\n");
    assert_eq!(e.render("Neighbor", &e.parse("Neighbor", &swapped).unwrap().value).unwrap(), CFG);
    let bare = data(json!({"peer": "10.0.0.1", "afs": [{"afi": "ipv6", "safi": "unicast"}]}));
    assert_eq!(e.render("Neighbor", &bare).unwrap(), "neighbor 10.0.0.1\n  address-family ipv6 unicast\n    exit-address-family\n");
    assert_eq!(e.parse("Neighbor", &e.render("Neighbor", &bare).unwrap()).unwrap().value, bare);
}

#[test]
fn missing_duplicate_and_extended_constants_fail() {
    let e = eng(AF, "nxos");
    let err = e.parse("Neighbor", &CFG.replace("    exit-address-family\n", "")).unwrap_err();
    assert!(err.0.contains("in `address-family ipv4 unicast`: constant line `exit-address-family` is missing (if this line is optional, declare a flag and write `exit-address-family [[ name ]]`)"), "{err}");
    let err = e.parse("Neighbor", &format!("{CFG}    exit-address-family\n")).unwrap_err();
    assert!(err.0.contains("`exit-address-family`: matched twice"), "{err}");
    let err = e.parse("Neighbor", &CFG.replace("    exit-address-family\n", "    exit-address-family now\n")).unwrap_err();
    assert!(err.0.contains("`exit-address-family now`: starts like a managed line"), "{err}");
    // `no <constant>` for a constant without the negation word is unrepresentable too.
    let err = e.parse("Neighbor", &CFG.replace("    exit-address-family\n", "    exit-address-family\n    no exit-address-family\n")).unwrap_err();
    assert!(err.0.contains("`no exit-address-family`: starts like a managed line"), "{err}");
}

#[test]
fn negated_constant_is_matched_literally() {
    let t = "model Dev\n  hostname: string\n\ntemplate Dev\n  hostname {{ hostname }}\n  no ip domain-lookup\n";
    let e = eng(t, "ios");
    let p = e.parse("Dev", "hostname r1\nno ip domain-lookup\n").unwrap();
    assert_eq!(p.value.to_json(), json!({"hostname": "r1"}));
    assert_eq!(e.render("Dev", &p.value).unwrap(), "hostname r1\nno ip domain-lookup\nend\n");
    let err = e.parse("Dev", "hostname r1\nip domain-lookup\n").unwrap_err();
    assert!(err.0.contains("`ip domain-lookup`: starts like a managed line"), "{err}");
    let err = e.parse("Dev", "hostname r1\n").unwrap_err();
    assert!(err.0.contains("constant line `no ip domain-lookup` is missing"), "{err}");
}

#[test]
fn constant_inside_a_junos_container() {
    let t = "model Sys\n  hostname: string?\n\ntemplate Sys\n  system {\n      host-name {{ hostname }};\n      services {\n          ssh;\n      }\n  }\n";
    let e = eng(t, "junos");
    let cfg = "system {\n    host-name r1;\n    services {\n        ssh;\n        netconf {\n            ssh;\n        }\n    }\n}\n";
    let p = e.parse("Sys", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"hostname": "r1"}));
    assert_eq!(p.unmanaged_paths(), vec!["system > services > netconf > ssh"]);
    let out = e.render("Sys", &p.value).unwrap();
    assert_eq!(out, "system {\n    host-name r1;\n    services {\n        ssh;\n    }\n}\n");
    // The constant is rendered even with no data, so its containers are too.
    assert_eq!(e.render("Sys", &data(json!({}))).unwrap(), "system {\n    services {\n        ssh;\n    }\n}\n");
    let err = e.parse("Sys", "system {\n    host-name r1;\n    services {\n        netconf;\n    }\n}\n").unwrap_err();
    assert!(err.0.contains("constant line `ssh` is missing"), "{err}");
    let err = e.parse("Sys", "system {\n    services {\n        ssh v2;\n    }\n}\n").unwrap_err();
    assert!(err.0.contains("`ssh v2`: starts like a managed line"), "{err}");
}

const FLAT: &str = "model N\n  peer: key ip\n  remoteAs: asn?\n\ntemplate N\n  neighbor {{ peer }} remote-as {{ remoteAs }}\n  neighbor {{ peer }} activate\n\nmodel B\n  asn: key asn\n  nbrs: [N]\n\ntemplate B\n  router bgp {{ asn }}\n    << nbrs >>\n";

#[test]
fn flat_group_constant_is_per_key() {
    let e = eng(FLAT, "eos");
    let cfg = "router bgp 1\n   neighbor 10.0.0.1 remote-as 2\n   neighbor 10.0.0.1 activate\n   neighbor 10.0.0.2 activate\n";
    let p = e.parse("B", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"asn": 1, "nbrs": [{"peer": "10.0.0.1", "remoteAs": 2}, {"peer": "10.0.0.2"}]}));
    assert_eq!(e.render("B", &p.value).unwrap(), cfg);
    let err = e.parse("B", "router bgp 1\n   neighbor 10.0.0.1 activate\n   neighbor 10.0.0.2 remote-as 3\n").unwrap_err();
    assert!(err.0.contains("N 10.0.0.2: constant line `neighbor {{ peer }} activate` is missing"), "{err}");
    let err = e.parse("B", "router bgp 1\n   neighbor 10.0.0.1 activate\n   neighbor 10.0.0.1 activate\n").unwrap_err();
    assert!(err.0.contains("matched twice"), "{err}");
    let err = e.parse("B", "router bgp 1\n   neighbor 10.0.0.1 activate\n   no neighbor 10.0.0.1 activate\n").unwrap_err();
    assert!(err.0.contains("`no neighbor 10.0.0.1 activate`: starts like a managed line"), "{err}");
    let err = e.parse("B", "router bgp 1\n   neighbor 10.0.0.1 activate now\n").unwrap_err();
    assert!(err.0.contains("starts like a managed line"), "{err}");
    assert!(e.explain("N").unwrap().contains("(constant)\n  always   → neighbor <peer:ip> activate\n"));
}

#[test]
fn root_body_constant() {
    // A dialect that doesn't skip `end`, so `end` is config the model can own.
    let t = "dialect plain\n  grammar: indent\n  indent: 2\n  negation: no\n\nmodel Device\n  hostname: string\n\ntemplate Device\n  hostname {{ hostname }}\n  end\n";
    let e = Engine::from_text("t.nct", t, None).unwrap();
    let p = e.parse("Device", "hostname r1\nend\n").unwrap();
    assert_eq!(p.value.to_json(), json!({"hostname": "r1"}));
    assert_eq!(e.render("Device", &p.value).unwrap(), "hostname r1\nend\n");
    let err = e.parse("Device", "hostname r1\n").unwrap_err();
    assert!(err.0.contains("constant line `end` is missing"), "{err}");
}

#[test]
fn explain_schema_skeleton_and_validation() {
    let e = eng(AF, "nxos");
    let x = e.explain("NeighborAf").unwrap();
    assert!(x.contains("(constant)\n  always   → exit-address-family\n"), "{x}");
    let s = e.schema("NeighborAf").unwrap();
    let props: Vec<&String> = s["$defs"]["NeighborAf"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(props, vec!["afi", "safi", "routeMapIn"]);
    assert_eq!(s["$defs"]["NeighborAf"]["required"], json!(["afi", "safi"]));
    let k = e.skeleton("Neighbor").unwrap();
    assert!(!k.contains("exit"), "{k}");
    let v = e.parse("Neighbor", CFG).unwrap().value;
    assert!(e.validate_data("Neighbor", &v).unwrap().is_empty());
}

#[test]
fn diff_writes_constants_only_with_whole_blocks() {
    let e = eng(AF, "nxos");
    let running = e.parse("Neighbor", CFG).unwrap().value;
    let intent = data(json!({"peer": "10.0.0.1", "afs": [
        {"afi": "ipv4", "safi": "unicast", "routeMapIn": "RM2"},
        {"afi": "ipv6", "safi": "unicast"},
    ]}));
    let text = e.diff("Neighbor", &running, &intent).unwrap().to_text().unwrap();
    assert_eq!(text, "neighbor 10.0.0.1\n  address-family ipv4 unicast\n    route-map RM2 in\n  address-family ipv6 unicast\n    exit-address-family\n");
    let text = e.diff("Neighbor", &running, &data(json!({"peer": "10.0.0.1", "afs": []}))).unwrap().to_text().unwrap();
    assert_eq!(text, "neighbor 10.0.0.1\n  no address-family ipv4 unicast\n");
    assert!(e.diff("Neighbor", &running, &running).unwrap().is_empty());
}

#[test]
fn lint_and_load_warnings() {
    // An @ignore covering a constant: the constant could never be matched.
    let t = AF.replace("    exit-address-family\n", "    exit-address-family\n    @ignore exit-address-family\n");
    let e = eng(&t, "nxos");
    assert!(e.warnings.iter().any(|w| w.contains("model NeighborAf: template line 9: `@ignore exit-address-family` covers the constant line `exit-address-family`")), "{:?}", e.warnings);
    assert!(e.lint(None).iter().any(|w| w.message.contains("covers the constant line")));
    let e = eng(&FLAT.replace("  neighbor {{ peer }} activate\n", "  neighbor {{ peer }} activate\n  @ignore neighbor * activate\n"), "eos");
    assert!(e.warnings.iter().any(|w| w.contains("covers the constant line `neighbor {{ peer }} activate`")), "{:?}", e.warnings);
    // A constant shadowed by an identical earlier line.
    let e = eng(&AF.replace("    exit-address-family\n", "    exit-address-family\n    exit-address-family\n"), "nxos");
    let w: Vec<String> = e.lint(None).iter().map(|w| w.to_string()).collect();
    assert!(w.iter().any(|w| w.contains("template line 10: `exit-address-family` is shadowed by `exit-address-family` (template line 9)")), "{w:#?}");
    assert!(eng(AF, "nxos").warnings.is_empty());
}

#[test]
fn validation_of_constant_lines() {
    let err = |t: &str, d: &str| Engine::from_text("t.nct", t, Some(d)).unwrap_err().0;
    // A body line with only a key is still misplaced, not a constant.
    let e = err(&AF.replace("    exit-address-family\n", "    family {{ afi }}\n"), "nxos");
    assert!(e.contains("Key field `afi` belongs on the header line"), "{e}");
    // Flat groups: constants carry the key, and cannot be negated.
    let e = err(&FLAT.replace("  neighbor {{ peer }} activate\n", "  activate\n"), "eos");
    assert!(e.contains("every line of a flat group must carry all Key fields"), "{e}");
    let e = err(&FLAT.replace("  neighbor {{ peer }} activate\n", "  no neighbor {{ peer }} activate\n"), "eos");
    assert!(e.contains("a constant line in a flat group cannot start with the negation word"), "{e}");
    // A malformed placeholder is still an error, not a constant.
    let e = err(&AF.replace("    exit-address-family\n", "    exit {{afi\n"), "nxos");
    assert!(e.contains("must be written {{ name }} (value)"), "{e}");
}
