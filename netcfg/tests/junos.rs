//! Brace grammar (Junos) through a declared dialect: containers, quoted strings, `set` output.

use netcfg::Engine;
use std::path::Path;

const CFG: &str = r#"## Last commit: 2026-09-24 12:00:00 UTC by admin
version 22.4R1;
system {
    host-name r1;
    /* inline note */
    services {
        ssh;
        netconf {
            ssh;
        }
    }
    syslog {
        user * {
            any emergency;
        }
    }
}
interfaces {
    ge-0/0/0 {
        description "to core";
        unit 0 {
            family inet {
                address 10.0.0.1/24;
            }
        }
    }
    lo0 {
        unit 0 {
            family inet {
                address 10.255.0.1/32;
            }
        }
    }
    apply-groups [ IFACE-DEFAULTS ];
}
protocols {
    bgp {
        group EXT {
            type external;
            neighbor 10.1.0.1 {
                description "peer one";
                peer-as 65001;
            }
            neighbor 10.1.0.2 {
                peer-as 65002;
                import POLICY-IN;
            }
        }
    }
    lldp {
        interface all;
    }
}
"#;

fn engine() -> Engine {
    Engine::load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/junos"), None).unwrap()
}

#[test]
fn parses_structured_config() {
    let e = engine();
    assert_eq!(e.dialect.name, "junos");
    let p = e.parse("JunosDevice", CFG).unwrap();
    let j = p.value.to_json();
    assert_eq!(j["hostname"], "r1");
    assert_eq!(j["sshEnabled"], true);
    assert_eq!(j["interfaces"][0]["name"], "ge-0/0/0");
    assert_eq!(j["interfaces"][0]["description"], "to core");
    assert_eq!(j["interfaces"][0]["address"], "10.0.0.1/24");
    assert_eq!(j["interfaces"][1]["address"], "10.255.0.1/32");
    assert_eq!(j["groups"][0]["type"], "external");
    assert_eq!(j["groups"][0]["neighbors"][0]["description"], "peer one");
    assert_eq!(j["groups"][0]["neighbors"][1]["peerAs"], 65002);
    assert_eq!(p.unmanaged_paths(), vec![
        "version 22.4R1",
        "system > services > netconf > ssh",
        "system > syslog > user * > any emergency",
        "interfaces > apply-groups [ IFACE-DEFAULTS ]",
        "protocols > bgp > group EXT > neighbor 10.1.0.2 > import POLICY-IN",
        "protocols > lldp > interface all",
    ]);
}

#[test]
fn renders_structured_and_set() {
    let e = engine();
    let p = e.parse("JunosDevice", CFG).unwrap();
    let out = e.render("JunosDevice", &p.value).unwrap();
    assert!(out.starts_with("system {\n    host-name r1;\n    services {\n        ssh;\n    }\n}\ninterfaces {\n    ge-0/0/0 {\n        description \"to core\";\n        unit 0 {\n            family inet {\n                address 10.0.0.1/24;\n            }\n        }\n    }\n"), "{out}");
    let again = e.parse("JunosDevice", &out).unwrap();
    assert_eq!(again.value, p.value);
    assert!(again.unmanaged.is_empty());

    let mut set = engine();
    set.dialect.render_set = true;
    let cmds = set.render("JunosDevice", &p.value).unwrap();
    assert!(cmds.contains("set system host-name r1\n"));
    assert!(cmds.contains("set interfaces ge-0/0/0 description \"to core\"\n"));
    assert!(cmds.contains("set interfaces ge-0/0/0 unit 0 family inet address 10.0.0.1/24\n"));
    assert!(cmds.contains("set protocols bgp group EXT neighbor 10.1.0.1 peer-as 65001\n"));
}

#[test]
fn strict_inside_containers() {
    let e = engine();
    let err = e.parse("JunosDevice", "system {\n    host-name r1 extra;\n}\n").unwrap_err();
    assert!(err.0.contains("host-name r1 extra"), "{err}");
    let err = e.parse("JunosDevice", "protocols {\n    bgp {\n        group EXT {\n            type weird;\n        }\n    }\n}\n").unwrap_err();
    assert!(err.0.contains("not one of internal|external"), "{err}");
    // An absent container just means every field in it is absent.
    let p = e.parse("JunosDevice", "interfaces {\n    lo0 {\n        description \"loopback\";\n    }\n}\n").unwrap();
    let j = p.value.to_json();
    assert_eq!(j["interfaces"][0]["description"], "loopback");
    assert!(j["interfaces"][0].get("address").is_none());
    assert_eq!(j["sshEnabled"], false);
}

#[test]
fn user_declared_dialect_variant() {
    let text = "dialect iosxe\n  extends: ios\n  skip: end, Building configuration, Current configuration, Load for\n\nmodel Host\n  hostname: string\n\ntemplate\n  hostname {{ hostname }}\n";
    let e = Engine::from_text("iosxe.ttp", text, None).unwrap();
    assert_eq!(e.dialect.name, "iosxe");
    let p = e.parse("Host", "Building configuration...\nLoad for five secs: 1%/0%\nhostname r1\nend\n").unwrap();
    assert_eq!(p.value.to_json()["hostname"], "r1");
    assert!(p.unmanaged.is_empty());
}
