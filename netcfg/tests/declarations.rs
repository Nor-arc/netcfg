//! Phase 2: docs, template comments, mapped enums, fragments, skeletons and data validation.

use netcfg::{Engine, Value};
use serde_json::json;
use std::path::Path;
use std::process::Command;

fn eng(text: &str, dialect: &str) -> Engine {
    Engine::from_text("t.nct", text, Some(dialect)).unwrap()
}
fn load_err(text: &str) -> String {
    Engine::from_text("t.nct", text, Some("nxos")).unwrap_err().0
}
fn data(j: serde_json::Value) -> Value {
    Value::from_json(&j)
}
fn tdir(d: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates").join(d)
}

// ---- 2.1 field documentation -----------------------------------------------------------------

const DOCS: &str = "# A BGP neighbor.
# One block per peer.
model Neighbor
  peer: key ip          # the peer's address
  remoteAs: asn?        # peer's AS; may be inherited from a peer template
  shutdown: flag        # administratively down

template Neighbor
  neighbor {{ peer }}
    remote-as {{ remoteAs }}
    shutdown [[ shutdown ]]
";

#[test]
fn docs_reach_schema_explain_and_skeleton() {
    let e = eng(DOCS, "nxos");
    let s = e.schema("Neighbor").unwrap();
    let n = &s["$defs"]["Neighbor"];
    assert_eq!(n["description"], "A BGP neighbor. One block per peer.");
    assert_eq!(n["properties"]["peer"]["description"], "the peer's address");
    assert_eq!(n["properties"]["remoteAs"]["description"], "peer's AS; may be inherited from a peer template");
    assert_eq!(n["properties"]["shutdown"]["description"], "administratively down");
    let x = e.explain("Neighbor").unwrap();
    assert!(x.starts_with("Neighbor (nxos dialect)\n# A BGP neighbor. One block per peer.\n"), "{x}");
    assert!(x.contains("remoteAs (optional)\n  # peer's AS; may be inherited from a peer template\n  value    → remote-as <remoteAs:asn>\n"), "{x}");
    assert!(x.contains("shutdown (flag, default false)\n  # administratively down\n"), "{x}");
    let k = e.skeleton("Neighbor").unwrap();
    assert!(k.contains("# A BGP neighbor. One block per peer.\n"), "{k}");
    assert!(k.contains("remoteAs: <asn>  # optional; peer's AS; may be inherited from a peer template\n"), "{k}");
}

#[test]
fn a_blank_line_detaches_a_comment() {
    let e = eng(&DOCS.replace("# One block per peer.\n", "# One block per peer.\n\n"), "nxos");
    assert!(e.schema("Neighbor").unwrap()["$defs"]["Neighbor"].get("description").is_none());
}

// ---- 2.2 comments inside template text -------------------------------------------------------

#[test]
fn double_hash_is_a_template_comment_single_hash_is_literal() {
    let t = "model M\n  desc: phrase?\n  mtu: int?\n\ntemplate M\n  ## Values are optional.\n  description # {{ desc }}\n    ## an indented comment between lines\n  mtu {{ mtu }}\n";
    let e = eng(t, "nxos");
    let p = e.parse("M", "description # to core\nmtu 9000\n").unwrap();
    assert_eq!(p.value.to_json(), json!({"desc": "to core", "mtu": 9000}));
    assert_eq!(e.render("M", &p.value).unwrap(), "description # to core\nmtu 9000\n");
    // Line numbers still refer to the file, past the comment lines.
    let err = load_err(&t.replace("mtu {{ mtu }}", "mtu {{ mtuu }}"));
    assert!(err.contains("template line 9 `mtu {{ mtuu }}`"), "{err}");
}

// ---- 2.3 enum with data mapping --------------------------------------------------------------

#[test]
fn mapped_enum() {
    let t = "type state = \"up\" -> true | \"down\" -> false\ntype speed = \"auto\" -> 0 | \"fast\" -> 100 | \"gig\" -> \"1g\"\n\nmodel P\n  name: key string\n  admin: state?\n  speed: speed?\n  match: {{ exact: \"exact-match\" -> true | \"\" -> false }} {{ names: list(string) }}?\n\ntemplate P\n  port {{ name }}\n    admin {{ admin }}\n    speed {{ speed }}\n    match {{ match }}\n";
    let e = eng(t, "nxos");
    let cfg = "port 1\n  admin up\n  speed gig\n  match exact-match A B\nport 2\n  admin down\n  speed auto\n  match A\n";
    let get = |c: &str| e.parse("P", c).map(|p| p.value.to_json());
    let first = cfg.split("port 2").next().unwrap();
    assert_eq!(get(first).unwrap(), json!({"name": "1", "admin": true, "speed": "1g", "match": {"names": ["A", "B"], "exact": true}}));
    let second = format!("port 2{}", cfg.split("port 2").nth(1).unwrap());
    assert_eq!(get(&second).unwrap(), json!({"name": "2", "admin": false, "speed": 0, "match": {"names": ["A"], "exact": false}}));
    for c in [first.to_string(), second.clone()] {
        let v = e.parse("P", &c).unwrap().value;
        assert_eq!(e.render("P", &v).unwrap(), c);
    }
    // Rendering picks the literal by value, and rejects unmapped values.
    assert_eq!(e.render("P", &data(json!({"name": "3", "speed": 100}))).unwrap(), "port 3\n  speed fast\n");
    let err = e.render("P", &data(json!({"name": "3", "admin": "up"}))).unwrap_err();
    assert!(err.0.contains("\"up\" is not a valid state (\"up\" -> true | \"down\" -> false)"), "{err}");
    let err = get("port 1\n  admin sideways\n").unwrap_err();
    assert!(err.0.contains("'sideways' is not a valid state"), "{err}");
    // Schema: the enum lists data values.
    let s = e.schema("P").unwrap();
    assert_eq!(s["$defs"]["P"]["properties"]["admin"]["anyOf"][0], json!({"enum": [true, false]}));
    assert_eq!(s["$defs"]["P"]["properties"]["speed"]["anyOf"][0], json!({"enum": [0, 100, "1g"]}));
}

#[test]
fn mapped_enum_errors() {
    let err = load_err("type s = \"up\" -> true | \"on\" -> true\nmodel M\n  a: s?\n\ntemplate M\n  a {{ a }}\n");
    assert!(err.contains("\"up\" and \"on\" both map to true"), "{err}");
    let err = load_err("type s = \"up\" | \"x\" -> \"up\"\nmodel M\n  a: s?\n\ntemplate M\n  a {{ a }}\n");
    assert!(err.contains("\"up\" and \"x\" both map to up"), "{err}");
    let err = load_err("type s = asn -> 1 | \"x\"\nmodel M\n  a: s?\n\ntemplate M\n  a {{ a }}\n");
    assert!(err.contains("`asn -> ...`: only a \"quoted\" literal can be mapped"), "{err}");
    let err = load_err("type s = \"x\" -> maybe\nmodel M\n  a: s?\n\ntemplate M\n  a {{ a }}\n");
    assert!(err.contains("`-> maybe`: a literal maps to true, false, an integer or a \"quoted string\""), "{err}");
}

// ---- 2.4 fragments ---------------------------------------------------------------------------

const FRAG: &str = "fragment CommonInterface
  description: phrase?
  mtu: int(576..9216) = 1500
  shutdown: flag

template CommonInterface
  description {{ description }}
  mtu {{ mtu }}
  shutdown [[ shutdown ]]

model Ethernet
  name: key string
  speed: int?
  address: cidr?

template Ethernet
  interface Ethernet{{ name }}
    speed {{ speed }}
    << @CommonInterface >>
    ip address {{ address }}

model Loopback
  id: key int

template Loopback
  interface loopback {{ id }}
    << @CommonInterface >>

model Device
  eth: [Ethernet]
  lo: [Loopback]

template Device
  << eth >>
  << lo >>
";

#[test]
fn fragments_splice_lines_and_fields() {
    let t = FRAG.replace("interface Ethernet{{ name }}", "interface {{ name }}");
    let e = eng(&t, "nxos");
    assert_eq!(e.model_names(), vec!["Ethernet", "Loopback", "Device"]);
    let cfg = "interface Ethernet1/1\n  speed 100\n  description uplink\n  mtu 9216\n  ip address 10.0.0.1/24\ninterface loopback 0\n  shutdown\n";
    let p = e.parse("Device", cfg).unwrap();
    let j = p.value.to_json();
    assert_eq!(j["eth"][0], json!({"name": "Ethernet1/1", "speed": 100, "description": "uplink", "mtu": 9216, "shutdown": false, "address": "10.0.0.1/24"}));
    // Fields are merged where the fragment is included, so data follows template order.
    assert_eq!(j["eth"][0].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["name", "speed", "description", "mtu", "shutdown", "address"]);
    assert_eq!(j["lo"][0], json!({"id": 0, "mtu": 1500, "shutdown": true}));
    assert_eq!(e.render("Device", &p.value).unwrap(), cfg);
    let s = e.schema("Device").unwrap();
    assert_eq!(s["$defs"]["Loopback"]["properties"]["mtu"]["maximum"], 9216);
    assert!(s["$defs"].get("CommonInterface").is_none());
    assert!(e.explain("Loopback").unwrap().contains("mtu (default 1500)"));
}

#[test]
fn fragment_errors() {
    let base = FRAG.replace("interface Ethernet{{ name }}", "interface {{ name }}");
    let err = load_err(&base.replace("  speed: int?\n", "  speed: int?\n  mtu: int?\n").replace("    speed {{ speed }}\n", "    speed {{ speed }}\n    jumbo {{ mtu }}\n"));
    assert!(err.contains("`<< @CommonInterface >>`: field `mtu` of fragment CommonInterface is already a field here"), "{err}");
    let err = load_err(&base.replace("<< @CommonInterface >>\n    ip", "<< @Common >>\n    ip"));
    assert!(err.contains("template line 19 `<< @Common >>`: no fragment Common (fragments: CommonInterface)"), "{err}");
    let err = load_err(&base.replace("<< @CommonInterface >>\n    ip", "<< @Loopback >>\n    ip"));
    assert!(err.contains("Loopback is a model, not a fragment; nest it with a field"), "{err}");
    let err = load_err(&base.replace("  shutdown [[ shutdown ]]\n", "  shutdown [[ shutdown ]]\n  << @CommonInterface >>\n"));
    assert!(err.contains("fragment CommonInterface line 10 `<< @CommonInterface >>`: fragments may not include other fragments"), "{err}");
    let err = load_err(&base.replace("  shutdown: flag\n", "  shutdown: flag\n  subs: [Loopback]\n").replace("  shutdown [[ shutdown ]]\n", "  shutdown [[ shutdown ]]\n  << subs >>\n"));
    assert!(err.contains("fragment CommonInterface: \n  - field `subs`: fragments may not contain nested models"), "{err}");
    let err = load_err(&base.replace("  description: phrase?\n", "  description: phrase?\n  id: key int\n").replace("  description {{ description }}\n", "  description {{ description }}\n  id {{ id }}\n"));
    assert!(err.contains("a fragment has no identity of its own, so it cannot declare keys"), "{err}");
    // Errors inside a fragment name the fragment and its own line.
    let err = load_err(&base.replace("  mtu {{ mtu }}\n", "  mtu {{ mtuu }}\n"));
    assert!(err.contains("fragment CommonInterface line 8 `mtu {{ mtuu }}`: `mtuu` is not a field of CommonInterface"), "{err}");
    let err = load_err(&base.replace("  speed: int?\n", "  speed: int?\n  common: CommonInterface?\n"));
    assert!(err.contains("`CommonInterface` is a fragment; include it with << @CommonInterface >> instead"), "{err}");
    let err = load_err(&base.replace("    << @CommonInterface >>\n    ip", "    x << @CommonInterface >>\n    ip"));
    assert!(err.contains("<< @CommonInterface >> must be alone on its line"), "{err}");
}

#[test]
fn nxos_example_uses_a_fragment() {
    let e = Engine::load_dir(&tdir("nxos"), None).unwrap();
    let cfg = "hostname h\nrouter bgp 1\n  template peer SPINE\n    remote-as 2\n    description spines\n    update-source lo0\n  neighbor 10.0.0.1\n    inherit peer SPINE\n    description s1\n    ebgp-multihop 2\n";
    let p = e.parse("Device", cfg).unwrap();
    let b = &p.value.to_json()["bgp"][0];
    assert_eq!(b["peerTemplates"][0], json!({"name": "SPINE", "remoteAs": 2, "description": "spines", "updateSource": "lo0"}));
    assert_eq!(b["neighbors"][0]["ebgpMultihop"], 2);
    assert_eq!(e.render("Device", &p.value).unwrap(), cfg);
}

// ---- 2.5 skeleton ----------------------------------------------------------------------------

#[test]
fn skeleton_is_yaml_with_placeholders() {
    let e = Engine::load_dir(&tdir("nxos"), None).unwrap();
    let k = e.skeleton("Device").unwrap();
    for frag in [
        "# null to write its negated form (`no ...`)",
        "hostname: <string>  # required\n",
        "routeMaps:  # list of RouteMapEntry\n  - name: <string>  # key\n    action: <action>  # required; \"permit\" | \"deny\"\n",
        "        ebgpMultihop: <int 2..255>  # optional\n",
        "        timers:  # optional\n          keepalive: <int>\n          hold: <int>\n",
        "          - afi: <string>  # key\n",
        "              threshold: <int 1..100>  # may be omitted\n",
        "    matchPrefixLists: [<string>]  # optional\n",
        "        shutdown: false  # flag, default false\n",
    ] {
        assert!(k.contains(frag), "missing {frag:?} in:\n{k}");
    }
    let y: serde_json::Value = serde_yaml::from_str(&k).unwrap();
    assert_eq!(y["bgp"][0]["neighbors"][0]["peer"], "<ip>");
    let ios = Engine::load_dir(&tdir("ios"), None).unwrap().skeleton("Interface").unwrap();
    assert!(ios.contains("mtu: 1500  # default; int 576..9216\n") && ios.contains("accessVlan: <vlanId>  # optional; /[1-9][0-9]{0,3}/\n"), "{ios}");
    let junos = Engine::load_dir(&tdir("junos"), None).unwrap().skeleton("JunosDevice").unwrap();
    assert!(junos.contains("# A value field: leave the key out to write nothing, or give a value to write it.\n"), "{junos}");
    let t = "model Ntp\n  server: ipv4\n\ntemplate Ntp\n  ntp server {{ server }}\n\nmodel D\n  ntp: Ntp?\n\ntemplate D\n  << ntp >>\n";
    assert!(eng(t, "nxos").skeleton("D").unwrap().contains("ntp:  # optional Ntp\n  server: <ipv4>  # required\n"));
}

// ---- 2.6 validate-data -----------------------------------------------------------------------

#[test]
fn validate_data_reports_every_error_with_render_messages() {
    let e = Engine::load_dir(&tdir("nxos"), None).unwrap();
    let bad = data(json!({
        "hostname": "h",
        "hostnme": "typo",
        "bgp": [{
            "asn": 1,
            "neighbors": [
                {"peer": "10.0.0.1", "remoteAs": "x", "shutdown": "yes"},
                {"peer": "10.0.0.1"},
                {"peer": "10.0.0.2", "timers": {"keepalive": 10}, "addressFamilies": [{"afi": "ipv4"}]},
            ],
        }],
        "routeMaps": [{"name": "RM", "seq": 10}],
    }));
    let errs: Vec<String> = e.validate_data("Device", &bad).unwrap().into_iter().map(|e| e.0).collect();
    let expect = [
        "Device: unknown field `hostnme` (Device fields: hostname, routeMaps, bgp)",
        "Device.routeMaps[0]: field `action` is missing",
        "Device.bgp[0].neighbors[0]: field `remoteAs`: \"x\" is not an AS number",
        "Device.bgp[0].neighbors[0]: field `shutdown`: expected true/false, got \"yes\"",
        "Device.bgp[0].neighbors[1]: duplicate Neighbor (same peer as neighbors[0])",
        "Device.bgp[0].neighbors[2]: field `timers`: Neighbor.timers: field `hold` is missing",
        "Device.bgp[0].neighbors[2].addressFamilies[0]: field `safi` is missing",
    ];
    assert_eq!(errs, expect, "{errs:#?}");
    // `render` stops at the first of the same messages.
    assert_eq!(e.render("Device", &bad).unwrap_err().0, expect[0]);
    // Valid data has no errors.
    let good = e.parse("Device", "hostname h\n").unwrap().value;
    assert!(e.validate_data("Device", &good).unwrap().is_empty());
    assert!(e.validate_data("Nope", &good).is_err());
    assert_eq!(e.validate_data("Device", &data(json!([1]))).unwrap()[0].0, "Device: expected a record, got [1]");
}

#[test]
fn validate_data_cli() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("validate_data");
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.yaml");
    std::fs::write(&bad, "hostname: h\nbgp:\n  - asn: 1\n    neighbors:\n      - peer: 10.0.0.1\n        remoteAs: x\n      - peer: nope\n").unwrap();
    let good = dir.join("good.json");
    std::fs::write(&good, "{\"hostname\": \"h\"}").unwrap();
    let t = tdir("nxos");
    let run = |f: &Path| Command::new(env!("CARGO_BIN_EXE_netcfg")).args(["validate-data", t.to_str().unwrap(), f.to_str().unwrap(), "--model", "Device"]).output().unwrap();
    let o = run(&bad);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(err.contains("error: Device.bgp[0].neighbors[0]: field `remoteAs`: \"x\" is not an AS number\n"), "{err}");
    assert!(err.contains("error: Device.bgp[0].neighbors[1]: field `peer`: \"nope\" is not a valid ip"), "{err}");
    assert!(err.contains("bad.yaml: 2 error(s)"), "{err}");
    let o = run(&good);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("is valid Device data"));
}
