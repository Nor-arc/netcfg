//! The edge-case suite: must-fail, must-be-unmanaged, must-be-invariant and must-parse-to
//! checks over small NX-OS and EOS configs. Ported from the Scala harness.

use netcfg::{Engine, Parsed, Value};
use std::path::Path;

fn engine(d: &str) -> Engine {
    Engine::load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates").join(d), Some(d)).unwrap()
}

struct Suite {
    e: Engine,
    model: &'static str,
    base: &'static str,
    base_parsed: Parsed,
}

impl Suite {
    fn new(d: &str, model: &'static str, base: &'static str) -> Suite {
        let e = engine(d);
        let base_parsed = e.parse(model, base).unwrap();
        Suite { e, model, base, base_parsed }
    }
    fn rep(&self, from: &str, to: &str) -> String {
        assert!(self.base.contains(from), "test bug: `{from}` not in base");
        self.base.replace(from, to)
    }
    fn append(&self, s: &str) -> String { format!("{}{}", self.base, s) }
    fn fails(&self, text: &str, fragment: &str) {
        match self.e.parse(self.model, text) {
            Err(e) => assert!(e.0.contains(fragment), "error lacks `{fragment}`: {e}"),
            Ok(p) => panic!("expected an error containing `{fragment}`, parsed: {:?}", p.value.to_json()),
        }
    }
    fn unmanaged(&self, text: &str, path: &str) -> Parsed {
        let p = self.e.parse(self.model, text).unwrap();
        let paths = p.unmanaged_paths();
        assert!(paths.iter().any(|x| x == path), "`{path}` not reported as unmanaged; got {paths:?}");
        p
    }
    fn invariant(&self, text: &str) {
        let p = self.e.parse(self.model, text).unwrap();
        assert_eq!(p.value, self.base_parsed.value);
        assert_eq!(p.unmanaged_paths(), self.base_parsed.unmanaged_paths());
    }
    fn parses_to(&self, text: &str, check: impl Fn(&serde_json::Value) -> bool) {
        let p = self.e.parse(self.model, text).unwrap();
        let j = p.value.to_json();
        assert!(check(&j), "model differs: {j}");
        assert_eq!(p.unmanaged_paths(), self.base_parsed.unmanaged_paths());
    }
    fn round_trip(&self, text: &str) {
        let p = self.e.parse(self.model, text).unwrap();
        let out = self.e.render(self.model, &p.value).unwrap();
        let again = self.e.parse(self.model, &out).unwrap();
        assert_eq!(again.value, p.value, "round trip changed the model");
        assert!(again.unmanaged.is_empty());
    }
}

const NX: &str = "!Command: show running-config
version 9.3(8)
hostname leaf1
route-map RM-IN permit 10
  match ip address prefix-list PL-1
  set local-preference 200
route-map RM-IN deny 20
router bgp 65000
  router-id 10.0.0.1
  neighbor 10.1.0.1
    remote-as 65001
    ebgp-multihop 2
    timers 10 30
    address-family ipv4 unicast
      route-map RM-IN in
";

const NBR: &str = "router bgp 65000 > neighbor 10.1.0.1";

#[test]
fn nxos_baseline() {
    let s = Suite::new("nxos", "Device", NX);
    let j = s.base_parsed.value.to_json();
    assert_eq!(j["routeMaps"][0]["matchPrefixLists"], serde_json::json!(["PL-1"]));
    assert_eq!(j["routeMaps"][1]["action"], "deny");
    assert_eq!(j["bgp"][0]["neighbors"][0]["timers"], serde_json::json!([10, 30]));
    assert_eq!(j["bgp"][0]["neighbors"][0]["addressFamilies"][0]["routeMapIn"], "RM-IN");
    assert_eq!(j["bgp"][0]["neighbors"][0]["shutdown"], false);
    assert_eq!(s.base_parsed.unmanaged_paths(), vec!["version 9.3(8)"]);
    s.round_trip(NX);
}

#[test]
fn nxos_must_fail() {
    let s = Suite::new("nxos", "Device", NX);
    s.fails(&s.rep("remote-as 65001", "remote-as abc"), "remote-as abc");
    s.fails(&s.rep("remote-as 65001", "remote-as route-map RM-AS"), "remote-as route-map RM-AS");
    s.fails(&s.rep("ebgp-multihop 2", "ebgp-multihop many"), "ebgp-multihop many");
    s.fails(&s.rep("ebgp-multihop 2", "ebgp-multihop 999"), "outside 2..255");
    s.fails(&s.rep("set local-preference 200", "set local-preference high"), "set local-preference high");
    s.fails(&s.rep("timers 10 30", "timers 10"), "timers 10");
    s.fails(&s.rep("router-id 10.0.0.1", "router-id 10.0.0.300"), "10.0.0.300");
    s.fails(&s.append("  neighbor 10.1.0.1\n    remote-as 65002\n"), "duplicate Neighbor");
    s.fails(&s.append("route-map RM-IN deny 10\n  set metric 5\n"), "duplicate RouteMapEntry");
    s.fails(&s.append("      send-community both\n"), "send-community both");
    s.fails(&s.append("      route-map RM-IN\n"), "route-map RM-IN");
    s.fails(&s.rep("ebgp-multihop 2", "ebgp-multihop 2 extra"), "ebgp-multihop 2 extra");
    s.fails(&s.append("    description one\n    description two\n"), "matched twice");
}

#[test]
fn nxos_must_be_unmanaged() {
    let s = Suite::new("nxos", "Device", NX);
    let p = s.unmanaged(&s.rep("route-map RM-IN deny 20", "route-map RM-IN deny twenty"), "route-map RM-IN deny twenty");
    assert_eq!(p.value.to_json()["routeMaps"].as_array().unwrap().len(), 1);
    s.unmanaged(&s.append("  neighbor 10.2.0.0/24\n    remote-as 65010\n"), "router bgp 65000 > neighbor 10.2.0.0/24 > remote-as 65010");
    s.unmanaged(&s.rep("    timers 10 30\n", "    timers 10 30\n    bfd\n"), &format!("{NBR} > bfd"));
    s.unmanaged(&s.append("  address-family ipv4 unicast\n    maximum-paths 4\n"), "router bgp 65000 > address-family ipv4 unicast > maximum-paths 4");
}

#[test]
fn nxos_invariants() {
    let s = Suite::new("nxos", "Device", NX);
    s.invariant(&s.rep("    timers 10 30\n", "    timers 10 30\n    no shutdown\n"));
    s.invariant(&s.rep("    remote-as 65001\n    ebgp-multihop 2\n    timers 10 30\n", "    timers 10 30\n    ebgp-multihop 2\n    remote-as 65001\n"));
    s.invariant(&NX.replace("  ", "    "));
    let with_noise: String = NX.lines().flat_map(|l| [l.to_string(), "".into(), "!".into()]).collect::<Vec<_>>().join("\n");
    s.invariant(&with_noise);
    s.invariant(&NX.replace('\n', "\r\n"));
}

#[test]
fn nxos_must_parse_to() {
    let s = Suite::new("nxos", "Device", NX);
    s.parses_to(&s.rep("    remote-as 65001\n", ""), |j| j["bgp"][0]["neighbors"][0].get("remoteAs").is_none());
    s.parses_to(&s.append("      maximum-prefix 1000 80 warning-only\n"), |j| j["bgp"][0]["neighbors"][0]["addressFamilies"][0]["maximumPrefix"] == serde_json::json!(["1000", "80", "warning-only"]));
    s.parses_to(&s.append("    inherit peer SPINE\n"), |j| j["bgp"][0]["neighbors"][0]["inheritPeer"] == "SPINE");
    s.parses_to(&s.rep("match ip address prefix-list PL-1", "match ip address prefix-list PL-1 PL-2"), |j| j["routeMaps"][0]["matchPrefixLists"] == serde_json::json!(["PL-1", "PL-2"]));
    s.parses_to(&s.rep("match ip address prefix-list PL-1", "match ip address ACL-1"), |j| j["routeMaps"][0]["matchAcl"] == "ACL-1");
    s.parses_to(&s.rep("route-map RM-IN permit 10", "route-map RM-IN deny 10"), |j| j["routeMaps"][0]["action"] == "deny" && j["routeMaps"][0]["seq"] == 10);
    s.parses_to(&s.append("  template peer SPINE\n    remote-as 65100\n"), |j| j["bgp"][0]["peerTemplates"][0]["remoteAs"] == 65100);
    s.parses_to(&s.rep("remote-as 65001", "remote-as 1.10"), |j| j["bgp"][0]["neighbors"][0]["remoteAs"] == 65546);
}

const EO: &str = "! Command: show running-config
hostname leaf1
router bgp 65000
   router-id 10.0.0.1
   neighbor 10.1.0.1 remote-as 65001
   neighbor 10.1.0.1 description to spine-1
   neighbor 10.1.0.1 route-map RM-IN in
   neighbor 10.1.0.1 send-community
   neighbor 10.1.0.2 remote-as 65002
   neighbor 10.1.0.2 shutdown
";

#[test]
fn eos_flat_groups() {
    let s = Suite::new("eos", "EosDevice", EO);
    let j = s.base_parsed.value.to_json();
    let n = j["bgp"][0]["neighbors"].as_array().unwrap();
    assert_eq!(n.len(), 2);
    assert_eq!(n[0]["description"], "to spine-1");
    assert_eq!(n[0]["sendCommunity"], true);
    assert_eq!(n[1]["shutdown"], true);
    assert!(s.base_parsed.unmanaged.is_empty());
    s.round_trip(EO);
    assert_eq!(s.e.render("EosDevice", &s.base_parsed.value).unwrap(),
        "hostname leaf1\nrouter bgp 65000\n   router-id 10.0.0.1\n   neighbor 10.1.0.1 remote-as 65001\n   neighbor 10.1.0.1 description to spine-1\n   neighbor 10.1.0.1 route-map RM-IN in\n   neighbor 10.1.0.1 send-community\n   neighbor 10.1.0.2 remote-as 65002\n   neighbor 10.1.0.2 shutdown\n");

    s.invariant(&s.rep("   neighbor 10.1.0.1 send-community\n   neighbor 10.1.0.2 remote-as 65002\n", "   neighbor 10.1.0.2 remote-as 65002\n   neighbor 10.1.0.1 send-community\n"));
    s.invariant(&s.append("   no neighbor 10.1.0.1 shutdown\n"));
    s.fails(&s.append("   neighbor 10.1.0.1 remote-as 65009\n"), "matched twice");
    s.fails(&s.append("   neighbor 10.1.0.1 send-community extended\n"), "send-community extended");
    s.fails(&s.append("   neighbor 10.1.0.1 remote-as x\n"), "not an AS number");
    s.parses_to(&s.append("   neighbor 10.1.0.3 description orphan\n"), |j| j["bgp"][0]["neighbors"][2]["peer"] == "10.1.0.3" && j["bgp"][0]["neighbors"][2].get("remoteAs").is_none());
    s.unmanaged(&s.append("   neighbor 10.1.0.1 password 7 abc\n"), "router bgp 65000 > neighbor 10.1.0.1 password 7 abc");
    s.unmanaged(&s.append("   neighbor 10.1.0.1 bfd\n"), "router bgp 65000 > neighbor 10.1.0.1 bfd");
    // An unknown line about an otherwise unknown neighbor must not conjure up a neighbor.
    let p = s.unmanaged(&s.append("   neighbor 10.1.0.9 bfd\n"), "router bgp 65000 > neighbor 10.1.0.9 bfd");
    assert_eq!(p.value, s.base_parsed.value);
    s.unmanaged(&s.append("   neighbor SPINE remote-as 65100\n"), "router bgp 65000 > neighbor SPINE remote-as 65100");
    // `no` on a value line is unrepresentable, so it is an error rather than silently dropped.
    s.fails(&s.append("   no neighbor 10.1.0.1 remote-as 65001\n"), "starts like a managed line");
}

#[test]
fn ignore_is_explicit_opt_in() {
    let strict = "model Logging\n  level: int?\n\ntemplate\n  logging level {{ level }}\n";
    let lenient = "model Logging\n  level: int?\n\ntemplate\n  logging level {{ level }}\n  @ignore logging level bgp\n";
    let text = "logging level 5\nlogging level bgp 3\n";
    let e = Engine::from_text("t", strict, Some("nxos")).unwrap();
    assert!(e.parse("Logging", text).unwrap_err().0.contains("logging level bgp 3"));
    let e = Engine::from_text("t", lenient, Some("nxos")).unwrap();
    let p = e.parse("Logging", text).unwrap();
    assert_eq!(p.value, Value::from_json(&serde_json::json!({"level": 5})));
    assert_eq!(p.unmanaged_paths(), vec!["logging level bgp 3"]);
}

#[test]
fn template_validation_reports_everything() {
    let bad = "model Iface\n  name: key string\n  description: phrase?\n  address: cidrx?\n  shutdown: flag\n\ntemplate\n  interface\n   name {{ name }}\n   description {{ descr }}\n   description {{ description }} end\n";
    let err = Engine::from_text("bad.ttp", bad, Some("ios")).unwrap_err().0;
    for frag in ["unknown type `cidrx`", "`descr` is not a field of Iface", "Key field `name` must appear on the header line", "consumes the rest of the line"] {
        assert!(err.contains(frag), "missing `{frag}` in:\n{err}");
    }
    let bad2 = "model Loose\n  x: int?\n\ntemplate\n  x {{ x }}\n\nmodel Holder\n  items: [Loose]\n\ntemplate\n  {{ items }}\n";
    let err = Engine::from_text("bad2.ttp", bad2, Some("ios")).unwrap_err().0;
    assert!(err.contains("needs at least one key field"), "{err}");
}

#[test]
fn union_and_list_types() {
    let s = Suite::new("nxos", "Device", NX);
    // asn | "auto", as a rest-of-line list
    s.parses_to(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend 65000 65000\n"), |j| j["routeMaps"][1]["prependAsPath"] == serde_json::json!([65000, 65000]));
    s.parses_to(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend auto\n"), |j| j["routeMaps"][1]["prependAsPath"] == serde_json::json!(["auto"]));
    s.parses_to(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend 1.10 auto\n"), |j| j["routeMaps"][1]["prependAsPath"] == serde_json::json!([65546, "auto"]));
    s.fails(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend banana\n"), "'banana' is not a valid prependItem (asn | \"auto\")");
    s.fails(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend 65000 banana\n"), "banana");
    s.round_trip(&s.rep("route-map RM-IN deny 20\n", "route-map RM-IN deny 20\n  set as-path prepend 65000 auto\n"));
    // rendering validates too
    let mut v = s.base_parsed.value.to_json();
    v["routeMaps"][1]["prependAsPath"] = serde_json::json!([65000, "nope"]);
    let err = s.e.render("Device", &Value::from_json(&v)).unwrap_err();
    assert!(err.0.contains("not a valid prependItem"), "{err}");
    // schema
    let sch = s.e.schema("Device").unwrap();
    assert_eq!(sch["$defs"]["RouteMapEntry"]["properties"]["prependAsPath"]["items"]["anyOf"][1]["enum"], serde_json::json!(["auto"]));
    // a bare unknown word in a union is an error, with a hint
    let err = Engine::from_text("t", "type x = asn | auto\nmodel M\n  a: x\n\ntemplate\n  a {{ a }}\n", Some("nxos")).unwrap_err();
    assert!(err.0.contains("unknown type `auto`") && err.0.contains("quote it"), "{err}");
}

#[test]
fn ipv6_neighbors() {
    let s = Suite::new("nxos", "Device", NX);
    let text = s.append("  neighbor 2001:DB8:0:0:0:0:0:1\n    remote-as 65010\n    address-family ipv6 unicast\n      route-map RM-IN in\n");
    s.parses_to(&text, |j| j["bgp"][0]["neighbors"][1]["peer"] == "2001:db8::1" && j["bgp"][0]["neighbors"][1]["addressFamilies"][0]["afi"] == "ipv6");
    s.round_trip(&text);
    let out = s.e.render("Device", &s.e.parse("Device", &text).unwrap().value).unwrap();
    assert!(out.contains("  neighbor 2001:db8::1\n    remote-as 65010\n"), "{out}");
    // Neither family: the header doesn't decode, so it is not ours.
    s.unmanaged(&s.append("  neighbor 2001:db8::zz\n    remote-as 1\n"), "router bgp 65000 > neighbor 2001:db8::zz > remote-as 1");
    let e = Engine::from_text("t", "model R\n  dst: key prefix\n  via: key ip\n\ntemplate\n  ip route {{ dst }} {{ via }}\n", Some("nxos")).unwrap();
    let p = e.parse("R", "ip route 2001:db8:1::/48 2001:db8::1\n").unwrap();
    assert_eq!(p.value.to_json(), serde_json::json!({"dst": "2001:db8:1::/48", "via": "2001:db8::1"}));
    assert!(e.parse("R", "ip route 2001:db8:1::/129 2001:db8::1\n").unwrap_err().0.contains("expected exactly one R"));
}

#[test]
fn optional_trailing_value_via_empty_literal() {
    let t = "type groupRef = string | \"\"\n\nmodel Nbr\n  peer: key ip\n  group: groupRef?\n  desc: phrase?\n\nmodel Bgp\n  asn: key asn\n  nbrs: [Nbr]\n\ntemplate\n  router bgp {{ asn }}\n    {{ nbrs }}\n";
    let t = t.replace("model Bgp", "template\n  neighbor {{ peer }} group {{ group }}\n  neighbor {{ peer }} description {{ desc }}\n\nmodel Bgp");
    let e = Engine::from_text("t", &t, Some("eos")).unwrap();
    let cfg = "router bgp 1\n   neighbor 10.0.0.1 group\n   neighbor 10.0.0.2 group CORE\n   neighbor 10.0.0.3 description no group line\n";
    let p = e.parse("Bgp", cfg).unwrap();
    let n = p.value.to_json()["nbrs"].clone();
    assert_eq!(n[0]["group"], "");
    assert_eq!(n[1]["group"], "CORE");
    assert!(n[2].get("group").is_none());
    assert!(p.unmanaged.is_empty());
    assert_eq!(e.render("Bgp", &p.value).unwrap(), cfg);
    // `""` must be the last placeholder on its line.
    let bad = "type g = string | \"\"\n\nmodel M\n  a: key string\n  b: g\n  c: int\n\ntemplate\n  x {{ a }}\n    y {{ b }} {{ c }}\n";
    let err = Engine::from_text("t", bad, Some("eos")).unwrap_err();
    assert!(err.0.contains("`b` consumes the rest of the line, so it must be last"), "{err}");
}
