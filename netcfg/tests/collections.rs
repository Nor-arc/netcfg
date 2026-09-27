//! Phase 3: positional collections and lists that stop before a literal.

use netcfg::{Engine, Value};
use serde_json::json;

fn eng(text: &str, dialect: &str) -> Engine {
    Engine::from_text("t.nct", text, Some(dialect)).unwrap()
}
fn load_err(text: &str) -> String {
    Engine::from_text("t.nct", text, Some("nxos")).unwrap_err().0
}

// ---- 3.1 positional collections --------------------------------------------------------------

const ACL: &str = "type action = \"permit\" | \"deny\"

model AclEntry
  action: action
  proto: string
  src: string
  dst: string

template AclEntry
  {{ action }} {{ proto }} {{ src }} {{ dst }}

model Acl
  name: key string
  entries: [AclEntry] ordered

template Acl
  ip access-list {{ name }}
    << entries >>
    @ignore statistics

model Device
  acls: [Acl]

template Device
  << acls >>
";

#[test]
fn positional_entries_keep_order_and_duplicates() {
    let e = eng(ACL, "nxos");
    let cfg = "ip access-list A\n  permit tcp any host1\n  deny ip any any\n  permit tcp any host1\n  statistics per-entry\n";
    let p = e.parse("Device", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"acls": [{"name": "A", "entries": [
        {"action": "permit", "proto": "tcp", "src": "any", "dst": "host1"},
        {"action": "deny", "proto": "ip", "src": "any", "dst": "any"},
        {"action": "permit", "proto": "tcp", "src": "any", "dst": "host1"},
    ]}]}));
    assert_eq!(p.unmanaged_paths(), vec!["ip access-list A > statistics per-entry"]);
    let out = e.render("Device", &p.value).unwrap();
    assert_eq!(out, cfg.replace("  statistics per-entry\n", ""));
    assert_eq!(e.parse("Device", &out).unwrap().value, p.value);
    // Data order is render order; duplicates are not an error.
    let v = Value::from_json(&json!({"acls": [{"name": "B", "entries": [
        {"action": "deny", "proto": "ip", "src": "any", "dst": "any"},
        {"action": "deny", "proto": "ip", "src": "any", "dst": "any"},
        {"action": "permit", "proto": "ip", "src": "any", "dst": "any"},
    ]}]}));
    assert!(e.validate_data("Device", &v).unwrap().is_empty());
    assert_eq!(e.render("Device", &v).unwrap(), "ip access-list B\n  deny ip any any\n  deny ip any any\n  permit ip any any\n");
    // Strictness is unchanged: a line starting with a value placeholder claims its level.
    let err = e.parse("Device", "ip access-list A\n  permit tcp any\n").unwrap_err();
    assert!(err.0.contains("permit tcp any"), "{err}");
    let err = e.parse("Device", "ip access-list A\n  allow tcp any any\n").unwrap_err();
    assert!(err.0.contains("'allow' is not a valid action"), "{err}");
}

#[test]
fn positional_keyed_blocks_allow_repeats() {
    let t = "model Step\n  seq: key int\n  note: phrase?\n\ntemplate Step\n  step {{ seq }}\n    note {{ note }}\n\nmodel Plan\n  steps: [Step] ordered\n  once: [Step]\n\ntemplate Plan\n  << steps >>\n";
    let err = load_err(t);
    assert!(err.contains("field `once` is not bound"), "{err}");
    let t = t.replace("  once: [Step]\n", "");
    let e = eng(&t, "nxos");
    let cfg = "step 2\n  note b\nstep 1\nstep 2\n  note again\n";
    let p = e.parse("Plan", cfg).unwrap();
    assert_eq!(p.value.to_json(), json!({"steps": [{"seq": 2, "note": "b"}, {"seq": 1}, {"seq": 2, "note": "again"}]}));
    assert_eq!(e.render("Plan", &p.value).unwrap(), cfg);
    let x = e.explain("Plan").unwrap();
    assert!(x.contains("steps (ordered list of Step)\n  each     → one Step block/line, in data order (identity is position; duplicates allowed)\n"), "{x}");
    assert_eq!(e.schema("Plan").unwrap()["$defs"]["Plan"]["properties"]["steps"]["type"], "array");
    assert!(e.skeleton("Plan").unwrap().contains("steps:  # ordered list of Step\n  - seq: <int>  # key\n"));
    // Without `ordered`, the same config is a duplicate.
    let e = eng(&t.replace("[Step] ordered", "[Step]"), "nxos");
    assert!(e.parse("Plan", cfg).unwrap_err().0.contains("duplicate Step: `step 2`"));
}

#[test]
fn positional_errors() {
    let err = load_err(&ACL.replace("[AclEntry] ordered", "[AclEntry]"));
    assert!(err.contains("model `AclEntry` needs at least one key field to be used in a collection (or make the collection positional: `[AclEntry] ordered`)"), "{err}");
    let err = load_err(&ACL.replace("  {{ action }} {{ proto }} {{ src }} {{ dst }}\n", "  {{ action }} {{ proto }}\n  to {{ src }} {{ dst }}\n"));
    assert!(err.contains("field `entries`: AclEntry has no key, so as a positional element it is identified by its header line; its template must have exactly one top-level line"), "{err}");
    let flat = "model N\n  peer: key ip\n  a: int?\n  b: int?\n\ntemplate N\n  neighbor {{ peer }} a {{ a }}\n  neighbor {{ peer }} b {{ b }}\n\nmodel B\n  ns: [N] ordered\n\ntemplate B\n  << ns >>\n";
    let err = load_err(flat);
    assert!(err.contains("field `ns`: N is a flat group, identified by its key; it cannot be a positional collection (drop `ordered`)"), "{err}");
}

// ---- 3.2 lists that stop before a literal ----------------------------------------------------

#[test]
fn list_stops_before_following_literal() {
    let t = "type communityMatch = {{ names: list(string) }} {{ exact: \"exact-match\" | \"\" }}\ntype dirList = {{ names: list(string) }} in\n\nmodel Rm\n  name: key string\n  community: communityMatch?\n  filters: dirList?\n\ntemplate Rm\n  route-map {{ name }}\n    match community {{ community }}\n    filter {{ filters }}\n";
    let e = eng(t, "nxos");
    let parse = |line: &str| e.parse("Rm", &format!("route-map RM\n  {line}\n")).map(|p| p.value.to_json());
    assert_eq!(parse("match community A B exact-match").unwrap()["community"], json!({"names": ["A", "B"], "exact": "exact-match"}));
    assert_eq!(parse("match community A B").unwrap()["community"], json!({"names": ["A", "B"]}));
    assert_eq!(parse("filter X Y in").unwrap()["filters"], json!({"names": ["X", "Y"]}));
    for line in ["match community A B exact-match", "match community A", "filter X in"] {
        let cfg = format!("route-map RM\n  {line}\n");
        assert_eq!(e.render("Rm", &e.parse("Rm", &cfg).unwrap().value).unwrap(), cfg);
    }
    // A token that is both a possible element and a stop word is the stop word.
    let err = parse("match community exact-match").unwrap_err();
    assert!(err.0.contains("communityMatch.names: expected one or more string"), "{err}");
    // Words after the stop word are not swallowed into the list.
    let err = parse("match community A exact-match B").unwrap_err();
    assert!(err.0.contains("match community A exact-match B"), "{err}");
    let err = parse("filter X Y").unwrap_err();
    assert!(err.0.contains("dirList: expected `in`, found end of line"), "{err}");
}

#[test]
fn list_stops_before_mapped_literal() {
    let t = "model Rm\n  name: key string\n  community: {{ names: list(string) }} {{ exact: \"exact-match\" -> true | \"\" -> false }}?\n\ntemplate Rm\n  route-map {{ name }}\n    match community {{ community }}\n";
    let e = eng(t, "nxos");
    let p = e.parse("Rm", "route-map RM\n  match community A B exact-match\n").unwrap();
    assert_eq!(p.value.to_json()["community"], json!({"names": ["A", "B"], "exact": true}));
    let p = e.parse("Rm", "route-map RM\n  match community A B\n").unwrap();
    assert_eq!(p.value.to_json()["community"], json!({"names": ["A", "B"], "exact": false}));
    assert_eq!(e.render("Rm", &p.value).unwrap(), "route-map RM\n  match community A B\n");
}

#[test]
fn ios_example_acl_is_positional() {
    let e = Engine::load_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/ios"), None).unwrap();
    let cfg = "hostname sw1\n!\nip access-list extended EDGE\n remark web\n permit tcp any any eq 443\n deny ip any any log\n permit tcp any any eq 443\n!\nend\n";
    let p = e.parse("Device", cfg).unwrap();
    assert_eq!(p.value.to_json()["acls"], json!([{"name": "EDGE", "entries": [
        {"action": "permit", "match": "tcp any any eq 443"},
        {"action": "deny", "match": "ip any any log"},
        {"action": "permit", "match": "tcp any any eq 443"},
    ]}]));
    assert_eq!(p.unmanaged_paths(), vec!["ip access-list extended EDGE > remark web"]);
    assert_eq!(e.render("Device", &p.value).unwrap(), cfg.replace(" remark web\n", "").replace("sw1\n!\n", "sw1\n"));
}
