//! Phase 5: change sets, the dialect `delete` word, render modes and provenance.

use netcfg::diff::{Change, DiffOptions};
use netcfg::lexer::OwnedNode;
use netcfg::{Engine, RenderMode, Value};
use serde_json::json;
use std::path::Path;
use std::process::Command;

fn set(d: &str) -> Engine {
    Engine::load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates").join(d), None).unwrap()
}
fn data(j: serde_json::Value) -> Value {
    Value::from_json(&j)
}

/// Apply a change set to a config tree the way an indent-dialect device does: a line with
/// `was` replaces that line, an entered header is found (by its old spelling if it changed),
/// deletes remove the statement (or, for a flat group, every line starting with it), and
/// anything else is appended. Negated lines stay in the config, as devices print them, so
/// re-parsing reads them back as `null`/`false`.
/// Lines are compared as text: a `phrase` value is one token when rendered, several when lexed.
fn apply(nodes: &mut Vec<OwnedNode>, changes: &[Change]) {
    let same = |n: &OwnedNode, l: &Vec<String>| n.line() == l.join(" ");
    for c in changes {
        match c {
            Change::Enter { line, was, children } => {
                let find = was.as_ref().unwrap_or(line);
                let at = match nodes.iter().position(|n| same(n, find)) {
                    Some(at) => at,
                    None => { nodes.push(OwnedNode::with_children(line.clone(), Vec::new())); nodes.len() - 1 }
                };
                nodes[at].tokens = line.clone();
                apply(&mut nodes[at].children, children);
            }
            Change::Set { node, was } => match was.as_ref().and_then(|w| nodes.iter().position(|n| same(n, w))) {
                Some(at) => nodes[at] = node.clone(),
                None => nodes.push(node.clone()),
            },
            Change::Delete { line, group } => {
                if *group { nodes.retain(|n| !(n.line() + " ").starts_with(&(line.join(" ") + " "))); }
                else if let Some(at) = nodes.iter().position(|n| same(n, line)) { nodes.remove(at); }
            }
        }
    }
}

/// Diff, apply the change set to the rendered running config, re-parse: must equal `expect`
/// (compared in canonical form, so a flag left out of `expect` is its default).
fn round_trip(e: &Engine, model: &str, running: &Value, intent: &Value, opts: DiffOptions, expect: &Value) -> String {
    let cs = e.diff_with(model, running, intent, opts).unwrap();
    let text = e.render(model, running).unwrap();
    let mut nodes: Vec<OwnedNode> = e.dialect.lex(&text).iter().map(OwnedNode::from_node).collect();
    apply(&mut nodes, &cs.changes);
    let applied = e.dialect.render(&nodes);
    let again = e.parse(model, &applied).unwrap_or_else(|err| panic!("{err}\n--- applied:\n{applied}"));
    let expect = e.parse(model, &e.render(model, expect).unwrap()).unwrap().value;
    assert_eq!(again.value.to_json(), expect.to_json(), "--- changes:\n{}--- applied:\n{applied}", cs.to_text().unwrap());
    // Applying again changes nothing.
    assert!(e.diff_with(model, &again.value, intent, opts).unwrap().is_empty(), "not idempotent");
    cs.to_text().unwrap()
}

const NX: &str = "hostname leaf1
route-map RM-IN permit 10
  match ip address prefix-list PL-1
  set local-preference 200
route-map RM-OLD deny 5
router bgp 65000
  router-id 10.0.0.1
  neighbor 10.1.0.1
    remote-as 65001
    description old
    ebgp-multihop 2
    address-family ipv4 unicast
      route-map RM-IN in
      send-community
  neighbor 10.1.0.2
    remote-as 65002
";

#[test]
fn nxos_add_remove_change() {
    let e = set("nxos");
    let running = e.parse("Device", NX).unwrap().value;
    let mut j = running.to_json();
    j["routeMaps"] = json!([{"name": "RM-IN", "action": "deny", "seq": 10, "matchPrefixLists": ["PL-1"], "setLocalPref": 200}]);
    let n1 = &mut j["bgp"][0]["neighbors"][0];
    n1["description"] = json!("new peer");
    n1["ebgpMultihop"] = json!(null);
    n1["shutdown"] = json!(true);
    n1["addressFamilies"][0]["sendCommunity"] = json!(false);
    let n1 = n1.clone();
    j["bgp"][0]["neighbors"] = json!([n1, {"peer": "10.1.0.3", "remoteAs": 65003, "addressFamilies": []}]);
    let intent = data(j);
    let text = round_trip(&e, "Device", &running, &intent, DiffOptions::default(), &intent);
    assert_eq!(text, "no route-map RM-OLD deny 5
route-map RM-IN deny 10
router bgp 65000
  no neighbor 10.1.0.2
  neighbor 10.1.0.1
    description new peer
    no ebgp-multihop
    shutdown
    address-family ipv4 unicast
      no send-community
  neighbor 10.1.0.3
    remote-as 65003
");
    let ops = e.diff("Device", &running, &intent).unwrap().to_json();
    assert_eq!(ops[1], json!({"op": "set", "path": [], "line": "route-map RM-IN deny 10", "was": "route-map RM-IN permit 10"}));
    assert_eq!(ops[2], json!({"op": "delete", "path": ["router bgp 65000"], "line": "neighbor 10.1.0.2"}));
    assert_eq!(ops[3], json!({"op": "set", "path": ["router bgp 65000", "neighbor 10.1.0.1"], "line": "description new peer", "was": "description old"}));
    assert_eq!(ops[4], json!({"op": "set", "path": ["router bgp 65000", "neighbor 10.1.0.1"], "line": "no ebgp-multihop", "was": "ebgp-multihop 2"}));
}

#[test]
fn identical_inputs_produce_an_empty_change_set() {
    for (d, model, cfg) in [("nxos", "Device", NX), ("eos", "EosDevice", EO), ("ios", "Device", "hostname sw1\ninterface Gi0/1\n shutdown\n no ip address\nip access-list extended A\n permit ip any any\n")] {
        let e = set(d);
        let v = e.parse(model, cfg).unwrap().value;
        let cs = e.diff(model, &v, &v).unwrap();
        assert!(cs.is_empty(), "{d}: {:?}", cs.changes);
        assert_eq!(cs.to_text().unwrap(), "");
        assert_eq!(cs.to_json(), json!([]));
    }
}

const EO: &str = "hostname leaf1
router bgp 65000
   router-id 10.0.0.1
   neighbor 10.1.0.1 remote-as 65001
   neighbor 10.1.0.1 description to spine-1
   neighbor 10.1.0.1 send-community
   neighbor 10.1.0.2 remote-as 65002
   neighbor 10.1.0.2 shutdown
";

#[test]
fn eos_flat_groups() {
    let e = set("eos");
    let running = e.parse("EosDevice", EO).unwrap().value;
    let mut j = running.to_json();
    let n1 = &mut j["bgp"][0]["neighbors"][0];
    n1["description"] = json!(null);
    n1["sendCommunity"] = json!(false);
    n1["maximumRoutes"] = json!({"limit": 1200, "action": "warning-only"});
    let n1 = n1.clone();
    j["bgp"][0]["neighbors"] = json!([n1, {"peer": "10.1.0.3", "remoteAs": 65003, "nextHopSelf": true}]);
    let intent = data(j);
    let text = round_trip(&e, "EosDevice", &running, &intent, DiffOptions::default(), &intent);
    assert_eq!(text, "router bgp 65000
   no neighbor 10.1.0.2
   no neighbor 10.1.0.1 description
   neighbor 10.1.0.1 maximum-routes 1200 warning-only
   no neighbor 10.1.0.1 send-community
   neighbor 10.1.0.3 remote-as 65003
   neighbor 10.1.0.3 next-hop-self
");
    let ops = e.diff("EosDevice", &running, &intent).unwrap().to_json();
    assert_eq!(ops[1], json!({"op": "set", "path": ["router bgp 65000"], "line": "no neighbor 10.1.0.1 description", "was": "neighbor 10.1.0.1 description to spine-1"}));
}

const IOS_T: &str = "model Interface
  name: key string
  switchport: flag = true
  address: cidr?
  mtu: int(576..9216) = 1500
  shutdown: flag = true

template Interface
  interface {{ name }}
   switchport [[ switchport ]]
   ip address {{ address }}
   mtu {{ mtu }}
   shutdown [[ shutdown ]]

model Dev
  ifaces: [Interface]

template Dev
  << ifaces >>
";

#[test]
fn flag_defaults_null_and_explicit() {
    let e = Engine::from_text("t", IOS_T, Some("ios")).unwrap();
    let running = e.parse("Dev", "interface Gi0/1\n no switchport\n ip address 10.0.0.1 255.255.255.0\n mtu 9000\n no shutdown\ninterface Gi0/2\n").unwrap().value;
    assert_eq!(running.to_json()["ifaces"][1], json!({"name": "Gi0/2", "switchport": true, "mtu": 1500, "shutdown": true}));
    // Flags go back to their device defaults with the positive/negated spelling; a defaulted
    // value goes back with its negated form; null clears a value.
    let intent = data(json!({"ifaces": [
        {"name": "Gi0/1", "switchport": true, "address": null, "mtu": 1500, "shutdown": true},
        {"name": "Gi0/2", "switchport": false, "address": "10.9.0.1/24", "mtu": 9216, "shutdown": false},
    ]}));
    let text = round_trip(&e, "Dev", &running, &intent, DiffOptions::default(), &intent);
    assert_eq!(text, "interface Gi0/1\n switchport\n no ip address\n no mtu\n shutdown\n!\ninterface Gi0/2\n no switchport\n ip address 10.9.0.1 255.255.255.0\n mtu 9216\n no shutdown\n!\n");

    // A missing key is no opinion; with `explicit` it is the absent/default state.
    let sparse = data(json!({"ifaces": [{"name": "Gi0/1"}, {"name": "Gi0/2"}]}));
    assert!(e.diff("Dev", &running, &sparse).unwrap().is_empty());
    let explicit = DiffOptions { explicit: true };
    let expect = data(json!({"ifaces": [
        {"name": "Gi0/1", "switchport": true, "address": null, "mtu": 1500, "shutdown": true},
        {"name": "Gi0/2", "switchport": true, "mtu": 1500, "shutdown": true},
    ]}));
    let text = round_trip(&e, "Dev", &running, &sparse, explicit, &expect);
    assert_eq!(text, "interface Gi0/1\n switchport\n no ip address\n no mtu\n shutdown\n!\n");
    // With explicit, a missing collection is empty.
    assert_eq!(e.diff_with("Dev", &running, &data(json!({})), explicit).unwrap().to_text().unwrap(), "no interface Gi0/1\nno interface Gi0/2\n");
    assert!(e.diff("Dev", &running, &data(json!({}))).unwrap().is_empty());
    // null newly present: running has nothing, intent null.
    let cs = e.diff("Dev", &data(json!({"ifaces": [{"name": "Gi0/3"}]})), &data(json!({"ifaces": [{"name": "Gi0/3", "address": null}]}))).unwrap();
    assert_eq!(cs.to_text().unwrap(), "interface Gi0/3\n no ip address\n!\n");
}

#[test]
fn intent_is_validated() {
    let e = set("nxos");
    let running = e.parse("Device", NX).unwrap().value;
    let err = e.diff("Device", &running, &data(json!({"hostname": "h", "bgp": [{"asn": 1, "neighbors": [{"peer": "x"}, {"peer": "10.0.0.1", "remoteAs": "y"}]}]}))).unwrap_err();
    assert!(err.0.starts_with("intent data is invalid:\n  Device.bgp[0].neighbors[0]: field `peer`"), "{err}");
    assert!(err.0.contains("\n  Device.bgp[0].neighbors[1]: field `remoteAs`"), "{err}");
}

#[test]
fn positional_collections_are_replaced_whole() {
    let e = set("ios");
    let running = e.parse("Device", "hostname sw1\nip access-list extended A\n permit tcp any any eq 443\n deny ip any any\n").unwrap().value;
    let mut j = running.to_json();
    j["acls"][0]["entries"] = json!([
        {"action": "permit", "match": "tcp any any eq 22"},
        {"action": "permit", "match": "tcp any any eq 443"},
        {"action": "deny", "match": "ip any any"},
    ]);
    let intent = data(j);
    let text = round_trip(&e, "Device", &running, &intent, DiffOptions::default(), &intent);
    assert_eq!(text, "ip access-list extended A\n no permit tcp any any eq 443\n no deny ip any any\n permit tcp any any eq 22\n permit tcp any any eq 443\n deny ip any any\n!\n");
    // Same entries: nothing.
    assert!(e.diff("Device", &running, &running).unwrap().is_empty());
}

#[test]
fn singletons_compare_presence() {
    let t = "model Ospf\n  pid: key int\n  routerId: ipv4?\n\ntemplate Ospf\n  router ospf {{ pid }}\n    router-id {{ routerId }}\n\nmodel Snmp\n  location: phrase?\n\ntemplate Snmp\n  snmp-server\n    location {{ location }}\n\nmodel D\n  ospf: Ospf?\n  snmp: Snmp?\n\ntemplate D\n  << ospf >>\n  << snmp >>\n";
    let e = Engine::from_text("t", t, Some("nxos")).unwrap();
    let running = e.parse("D", "router ospf 1\n  router-id 1.1.1.1\nsnmp-server\n  location a\n").unwrap().value;
    let d = |j| e.diff("D", &running, &data(j)).unwrap().to_text().unwrap();
    assert_eq!(d(json!({"ospf": {"pid": 1, "routerId": "2.2.2.2"}})), "router ospf 1\n  router-id 2.2.2.2\n");
    assert_eq!(d(json!({"ospf": {"pid": 2}, "snmp": null})), "no router ospf 1\nno snmp-server\nrouter ospf 2\n");
    assert_eq!(d(json!({"snmp": {"location": "b"}})), "snmp-server\n  location b\n");
    let intent = data(json!({"ospf": {"pid": 2, "routerId": "2.2.2.2"}, "snmp": {"location": "b"}}));
    round_trip(&e, "D", &running, &intent, DiffOptions::default(), &intent);
}

#[test]
fn junos_set_style_deletion() {
    let mut e = set("junos");
    let cfg = "system {\n    host-name r1;\n    services {\n        ssh;\n    }\n}\nprotocols {\n    bgp {\n        group EXT {\n            type external;\n            neighbor 10.1.0.1 {\n                peer-as 65001;\n            }\n            neighbor 10.1.0.2 {\n                description \"to two\";\n                peer-as 65002;\n            }\n        }\n    }\n}\n";
    let running = e.parse("JunosDevice", cfg).unwrap().value;
    let mut j = running.to_json();
    j["sshEnabled"] = json!(false);
    j["groups"][0]["neighbors"] = json!([{"peer": "10.1.0.1", "peerAs": 65009, "description": "new one"}]);
    let intent = data(j);
    // Structured rendering has no deletion form.
    let err = e.diff("JunosDevice", &running, &intent).unwrap_err();
    assert!(err.0.contains("structured braces rendering has no deletion form; `diff` needs `render: set`"), "{err}");
    e.dialect.render_set = true;
    assert_eq!(e.dialect.delete_word(), Some("delete"));
    let text = e.diff("JunosDevice", &running, &intent).unwrap().to_text().unwrap();
    assert_eq!(text, "delete system services ssh\ndelete protocols bgp group EXT neighbor 10.1.0.2\nset protocols bgp group EXT neighbor 10.1.0.1 description \"new one\"\nset protocols bgp group EXT neighbor 10.1.0.1 peer-as 65009\n");
    // A removed value (no negation in Junos) is a delete of its line.
    let mut k = running.to_json();
    k["hostname"] = json!(null);
    let err = e.diff("JunosDevice", &running, &data(k)).unwrap_err();
    assert!(err.0.contains("null has no spelling in this dialect"), "{err}");
    let mut k = running.to_json();
    k["groups"][0]["neighbors"][1].as_object_mut().unwrap().remove("description");
    assert!(e.diff("JunosDevice", &running, &data(k.clone())).unwrap().is_empty());
    let text = e.diff_with("JunosDevice", &running, &data(k), DiffOptions { explicit: true }).unwrap().to_text().unwrap();
    assert_eq!(text, "delete protocols bgp group EXT neighbor 10.1.0.2 description \"to two\"\n");
}

// ---- 5.3 render modes ------------------------------------------------------------------------

#[test]
fn explicit_render_writes_defaults() {
    let e = Engine::from_text("t", IOS_T, Some("ios")).unwrap();
    let v = data(json!({"ifaces": [{"name": "Gi0/1"}, {"name": "Gi0/2", "switchport": false, "mtu": 9000, "shutdown": false}]}));
    assert_eq!(e.render("Dev", &v).unwrap(), "interface Gi0/1\n!\ninterface Gi0/2\n no switchport\n mtu 9000\n no shutdown\n!\nend\n");
    let x = e.render_with("Dev", &v, RenderMode::Explicit).unwrap();
    assert_eq!(x, "interface Gi0/1\n switchport\n mtu 1500\n shutdown\n!\ninterface Gi0/2\n no switchport\n mtu 9000\n no shutdown\n!\nend\n");
    // Explicit output parses to the same data as canonical output.
    assert_eq!(e.parse("Dev", &x).unwrap().value, e.parse("Dev", &e.render("Dev", &v).unwrap()).unwrap().value);
    // A false flag with no spelling (no negation word) is simply not written.
    let j = set("junos");
    let jv = j.parse("JunosDevice", "system {\n    host-name r1;\n}\n").unwrap().value;
    assert_eq!(j.render_with("JunosDevice", &jv, RenderMode::Explicit).unwrap(), j.render("JunosDevice", &jv).unwrap());
}

// ---- CLI and provenance ----------------------------------------------------------------------

fn netcfg(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_netcfg")).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn cli_diff_render_and_provenance() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("diff_cli");
    std::fs::create_dir_all(&dir).unwrap();
    let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/nxos");
    let (t, run, parsed, intent) = (t.to_str().unwrap(), dir.join("run.cfg"), dir.join("parsed.json"), dir.join("intent.yaml"));
    std::fs::write(&run, format!("{NX}    bfd\n")).unwrap();
    let (ok, out, err) = netcfg(&["parse", t, run.to_str().unwrap(), "--model", "Device", "--format", "json"]);
    assert!(ok, "{err}");
    let env: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(env["engine_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(env["templates_version"], "2026.09.1");
    assert_eq!(env["model"], "Device");
    assert_eq!(env["unmanaged"], json!(["router bgp 65000 > neighbor 10.1.0.2 > bfd"]));
    std::fs::write(&parsed, &out).unwrap();
    let (ok, out, _) = netcfg(&["parse", t, run.to_str().unwrap(), "--model", "Device"]);
    assert!(ok && out.starts_with(&format!("# Device: netcfg {}, templates 2026.09.1\nhostname: leaf1\n", env!("CARGO_PKG_VERSION"))), "{out}");
    // The envelope reads back as data.
    let (ok, out, err) = netcfg(&["render", t, parsed.to_str().unwrap(), "--model", "Device"]);
    assert!(ok, "{err}");
    assert_eq!(out, NX.replace("      route-map RM-IN in\n      send-community\n", "      send-community\n      route-map RM-IN in\n"));
    let (ok, out, _) = netcfg(&["render", t, parsed.to_str().unwrap(), "--model", "Device", "--explicit"]);
    assert!(ok && out.contains("    no shutdown\n") && out.contains("  no log-neighbor-changes\n"), "{out}");
    // Diff.
    std::fs::write(&intent, "hostname: leaf2\n").unwrap();
    let (ok, out, err) = netcfg(&["diff", t, run.to_str().unwrap(), intent.to_str().unwrap(), "--model", "Device", "--show-unmanaged"]);
    assert!(ok, "{err}");
    assert_eq!(out, "hostname leaf2\n");
    assert_eq!(err, "unmanaged: router bgp 65000 > neighbor 10.1.0.2 > bfd\n");
    let (ok, out, _) = netcfg(&["diff", "--set", &format!("{t}/set.nct"), run.to_str().unwrap(), intent.to_str().unwrap(), "--model", "Device", "--format", "json"]);
    let j: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(ok);
    assert_eq!(j["templates_version"], "2026.09.1");
    assert_eq!(j["changes"], json!([{"op": "set", "path": [], "line": "hostname leaf2", "was": "hostname leaf1"}]));
    let (ok, out, _) = netcfg(&["diff", t, run.to_str().unwrap(), parsed.to_str().unwrap(), "--model", "Device"]);
    assert!(ok && out.is_empty(), "{out}");
    std::fs::write(&intent, "hostname: 5\nbogus: 1\n").unwrap();
    let (ok, _, err) = netcfg(&["diff", t, run.to_str().unwrap(), intent.to_str().unwrap(), "--model", "Device"]);
    assert!(!ok && err.contains("intent data is invalid") && err.contains("unknown field `bogus`"), "{err}");
}
