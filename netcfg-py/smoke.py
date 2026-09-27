"""Smoke test for the Python bindings. Run after installing the wheel:

    cd netcfg-py && maturin build --release && pip install ../target/wheels/netcfg-*.whl
    python smoke.py
"""
import pathlib
import warnings

import netcfg

ROOT = pathlib.Path(__file__).resolve().parent.parent
T = ROOT / "templates"

print("netcfg", netcfg.__version__, netcfg.__build__)

# Parse / render round trip on the NX-OS example set.
e = netcfg.Engine(str(T / "nxos"))
assert e.dialect == "nxos", e.dialect
cfg = """hostname leaf1
router bgp 65000
  router-id 10.0.0.1
  neighbor 10.1.0.1
    remote-as 65001
    timers 10 30
    bfd
"""
r = e.parse("Device", cfg)
nbr = r.value["bgp"][0]["neighbors"][0]
assert nbr["timers"] == {"keepalive": 10, "hold": 30}, nbr
assert r.unmanaged == ["router bgp 65000 > neighbor 10.1.0.1 > bfd"], r.unmanaged
assert e.render("Device", r.value) == cfg.replace("    bfd\n", "")
assert e.schema("Device")["$defs"]["Neighbor"]["properties"]["timers"]["anyOf"][0]["type"] == "object"
try:
    e.parse("Device", cfg.replace("remote-as 65001", "remote-as abc"))
    raise AssertionError("expected ValueError")
except ValueError as err:
    assert "remote-as abc" in str(err)

# Singletons and the deprecated bare template.
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    s = netcfg.Engine.from_text(
        "model Ntp\n  server: ipv4\n\ntemplate\n  ntp server {{ server }}\n\n"
        "model D\n  ntp: Ntp?\n\ntemplate D\n  << ntp >>\n",
        "nxos",
    )
assert any(issubclass(w.category, DeprecationWarning) and "bare `template`" in str(w.message) for w in caught), caught
assert s.warnings and "template Ntp" in s.warnings[0]
assert s.parse("D", "ntp server 10.0.0.1\n").value == {"ntp": {"server": "10.0.0.1"}}
assert s.render("D", {}) == ""

# Skeleton and data validation.
sk = e.skeleton("Device")
assert "ebgpMultihop: <int 2..255>  # optional" in sk, sk
errs = e.validate_data("Device", {"hostname": "h", "bgp": [{"asn": 1, "neighbors": [{"peer": "10.0.0.1", "remoteAs": "x"}, {"peer": "nope"}]}]})
assert len(errs) == 2 and "Device.bgp[0].neighbors[0]: field `remoteAs`" in errs[0], errs
assert e.validate_data("Device", r.value) == []

# Sets and provenance.
assert e.set_name == "nxos" and e.templates_version == "2026.09.1"
assert r.templates_version == "2026.09.1" and r.engine_version == netcfg.__version__
sets = netcfg.load_all(str(T))
assert sorted(sets) == ["eos", "ios", "junos", "nxos"], sets
assert netcfg.Engine(str(T / "eos" / "set.nct")).set_name == "eos"
assert s.set_name is None and s.parse("D", "").templates_version == "unversioned"

# Change sets and render modes.
intent = dict(r.value, hostname="leaf2")
cs = e.diff("Device", r.value, intent)
assert cs.text == "hostname leaf2\n" and not cs.empty, cs.text
assert cs.ops == [{"op": "set", "path": [], "line": "hostname leaf2", "was": "hostname leaf1"}], cs.ops
assert e.diff("Device", r.value, r.value).empty
assert "    no shutdown\n" in e.render("Device", r.value, explicit=True)

print("ok")
