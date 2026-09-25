use netcfg::{Engine, Value};
use std::path::Path;

fn tdir(d: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates").join(d)
}

#[test]
fn ios_round_trip() {
    let e = Engine::load_dir(&tdir("ios"), Some("ios")).unwrap();
    let cfg = "hostname sw1\n!\nvlan 10\n name users\n!\ninterface Gi0/1\n description uplink to core\n ip address 10.0.0.1 255.255.255.0\n ip address 10.0.1.1 255.255.255.0 secondary\n mtu 9000\n shutdown\n!\ninterface Gi0/2\n switchport access vlan 10\n spanning-tree portfast\n!\nip route 0.0.0.0 0.0.0.0 10.0.0.254\nline vty 0 4\n transport input ssh\nend\n";
    let p = e.parse("Device", cfg).unwrap();
    let j = p.value.to_json();
    assert_eq!(j["hostname"], "sw1");
    assert_eq!(j["interfaces"][0]["address"], "10.0.0.1/24");
    assert_eq!(j["interfaces"][0]["mtu"], 9000);
    assert_eq!(j["interfaces"][1]["mtu"], 1500);
    assert_eq!(j["interfaces"][0]["shutdown"], true);
    assert_eq!(j["routes"][0]["prefix"], "0.0.0.0/0");
    let paths = p.unmanaged_paths();
    assert_eq!(paths, vec![
        "interface Gi0/1 > ip address 10.0.1.1 255.255.255.0 secondary",
        "interface Gi0/2 > spanning-tree portfast",
        "line vty 0 4 > transport input ssh",
    ]);
    let out = e.render("Device", &p.value).unwrap();
    let again = e.parse("Device", &out).unwrap();
    assert_eq!(again.value, p.value);
    assert!(again.unmanaged.is_empty());
    assert!(out.contains("interface Gi0/1\n description uplink to core\n ip address 10.0.0.1 255.255.255.0\n mtu 9000\n shutdown\n!\n"));
}

#[test]
fn strictness_and_ignore() {
    let e = Engine::load_dir(&tdir("ios"), Some("ios")).unwrap();
    let err = e.parse("Device", "hostname h\ninterface Gi0/1\n mtu 99999\n").unwrap_err();
    assert!(err.0.contains("99999 is outside 576..9216"), "{err}");
    let err = e.parse("Device", "hostname h\ninterface Gi0/1\n ip address dhcp\n").unwrap_err();
    assert!(err.0.contains("netmask"), "{err}");
    let err = e.parse("Device", "hostname h\ninterface Gi0/1\n mtu 1500 bytes\n").unwrap_err();
    assert!(err.0.contains("starts like a managed line"), "{err}");
}

#[test]
fn schema_is_generated() {
    let e = Engine::load_dir(&tdir("ios"), Some("ios")).unwrap();
    let s = e.schema("Device").unwrap();
    assert_eq!(s["$defs"]["Interface"]["properties"]["mtu"]["minimum"], 576);
    assert_eq!(s["$defs"]["Interface"]["required"], serde_json::json!(["name"]));
    assert_eq!(s["$defs"]["Vlan"]["properties"]["id"]["pattern"], "^[1-9][0-9]{0,3}$");
}

#[test]
fn data_from_yaml_renders() {
    let e = Engine::load_dir(&tdir("nxos"), Some("nxos")).unwrap();
    let y: serde_json::Value = serde_yaml::from_str("hostname: leaf1\nrouteMaps: []\nbgp:\n  - asn: 65000\n    routerId: 10.0.0.1\n    peerTemplates: []\n    neighbors:\n      - peer: 10.1.0.1\n        remoteAs: 65001\n        addressFamilies:\n          - {afi: ipv4, safi: unicast, routeMapIn: RM-IN, sendCommunity: true}\n").unwrap();
    let out = e.render("Device", &Value::from_json(&y)).unwrap();
    assert_eq!(out, "hostname leaf1\nrouter bgp 65000\n  router-id 10.0.0.1\n  neighbor 10.1.0.1\n    remote-as 65001\n    address-family ipv4 unicast\n      send-community\n      route-map RM-IN in\n");
}
