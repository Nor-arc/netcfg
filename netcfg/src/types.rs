//! The scalar type catalog. Templates name types (`ipv4`, `asn`, `phrase`); each type knows
//! how to read tokens from a line and how to write a value back.

use crate::value::Value;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;

pub trait Scalar: Send + Sync {
    fn name(&self) -> &str;
    /// Consumes the rest of the line; such a placeholder must be last.
    fn rest_of_line(&self) -> bool {
        false
    }
    /// Read a value from the start of `toks`, returning it and how many tokens were used.
    fn parse(&self, toks: &[&str]) -> Result<(Value, usize), String>;
    fn encode(&self, v: &Value) -> Result<Vec<String>, String>;
    /// JSON Schema fragment for the value.
    fn schema(&self) -> serde_json::Value;
    fn describe(&self) -> String {
        self.name().to_string()
    }
}

pub type ScalarRef = Arc<dyn Scalar>;

fn one<'a>(toks: &[&'a str], what: &str) -> Result<&'a str, String> {
    toks.first().copied().ok_or_else(|| format!("expected {what}, found end of line"))
}
fn expect_str<'a>(v: &'a Value, ty: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("expected a string for {ty}, got {v:?}"))
}
fn json_str(pattern: Option<&str>, description: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "string".into());
    if let Some(p) = pattern {
        m.insert("pattern".into(), p.into());
    }
    m.insert("description".into(), description.into());
    serde_json::Value::Object(m)
}

// ---- builtins ----------------------------------------------------------------------------

struct Str;
impl Scalar for Str {
    fn name(&self) -> &str { "string" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> { Ok((Value::Str(one(t, "a word")?.to_string()), 1)) }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> { Ok(vec![expect_str(v, "string")?.to_string()]) }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^\S+$"), "one word") }
}

struct Int { min: i64, max: i64 }
impl Scalar for Int {
    fn name(&self) -> &str { "int" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an integer")?;
        let i: i64 = w.parse().map_err(|_| format!("'{w}' is not an integer"))?;
        if i < self.min || i > self.max { return Err(format!("{i} is outside {}..{}", self.min, self.max)); }
        Ok((Value::Int(i), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        match v { Value::Int(i) => Ok(vec![i.to_string()]), _ => Err(format!("expected an integer, got {v:?}")) }
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "integer", "minimum": self.min, "maximum": self.max})
    }
}

struct Ipv4;
fn parse_ipv4(s: &str) -> Option<u32> {
    let mut parts = s.split('.');
    let mut v: u32 = 0;
    for _ in 0..4 {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) { return None; }
        let n: u32 = p.parse().ok()?;
        if n > 255 { return None; }
        v = (v << 8) | n;
    }
    if parts.next().is_some() { return None; }
    Some(v)
}
fn fmt_ipv4(v: u32) -> String { format!("{}.{}.{}.{}", v >> 24, (v >> 16) & 255, (v >> 8) & 255, v & 255) }
impl Scalar for Ipv4 {
    fn name(&self) -> &str { "ipv4" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an IPv4 address")?;
        let v = parse_ipv4(w).ok_or_else(|| format!("'{w}' is not an IPv4 address"))?;
        Ok((Value::Str(fmt_ipv4(v)), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, "ipv4")?;
        parse_ipv4(s).map(|a| vec![fmt_ipv4(a)]).ok_or_else(|| format!("'{s}' is not an IPv4 address"))
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^(\d{1,3}\.){3}\d{1,3}$"), "IPv4 address") }
}

/// `addr/len`, or `addr mask` when `masked`. The value is always `addr/len`.
struct Cidr { masked: bool }
fn mask_to_len(m: u32) -> Option<u32> {
    let len = m.leading_ones();
    if m == (u32::MAX.checked_shl(32 - len).unwrap_or(0)) { Some(len) } else { None }
}
fn len_to_mask(len: u32) -> u32 { if len == 0 { 0 } else { u32::MAX << (32 - len) } }
impl Scalar for Cidr {
    fn name(&self) -> &str { "cidr" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if self.masked {
            if t.len() < 2 { return Err("expected IPv4 address + netmask (two tokens)".into()); }
            let a = parse_ipv4(t[0]).ok_or_else(|| format!("'{}' is not an IPv4 address", t[0]))?;
            let m = parse_ipv4(t[1]).ok_or_else(|| format!("'{}' is not a netmask", t[1]))?;
            let len = mask_to_len(m).ok_or_else(|| format!("'{}' is not a contiguous netmask", t[1]))?;
            Ok((Value::Str(format!("{}/{}", fmt_ipv4(a), len)), 2))
        } else {
            let w = one(t, "an IPv4 prefix")?;
            let (a, l) = w.split_once('/').ok_or_else(|| format!("'{w}' is not an IPv4 prefix (addr/len)"))?;
            let a = parse_ipv4(a).ok_or_else(|| format!("'{w}' is not an IPv4 prefix"))?;
            let l: u32 = l.parse().ok().filter(|l| *l <= 32).ok_or_else(|| format!("'{w}' has an invalid prefix length"))?;
            Ok((Value::Str(format!("{}/{}", fmt_ipv4(a), l)), 1))
        }
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, "cidr")?;
        let (a, l) = s.split_once('/').ok_or_else(|| format!("'{s}' is not addr/len"))?;
        let a = parse_ipv4(a).ok_or_else(|| format!("'{s}' is not addr/len"))?;
        let l: u32 = l.parse().ok().filter(|l| *l <= 32).ok_or_else(|| format!("'{s}' has an invalid prefix length"))?;
        Ok(if self.masked { vec![fmt_ipv4(a), fmt_ipv4(len_to_mask(l))] } else { vec![format!("{}/{}", fmt_ipv4(a), l)] })
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^(\d{1,3}\.){3}\d{1,3}/\d{1,2}$"), "IPv4 prefix, addr/len") }
}

/// IPv6 address, canonicalised per RFC 5952 (lowercase, longest zero run compressed).
struct Ipv6;
fn parse_ipv6(s: &str) -> Option<std::net::Ipv6Addr> {
    // Reject IPv4-mapped/compat spellings that std accepts but devices don't print.
    if s.contains('%') { return None; }
    s.parse::<std::net::Ipv6Addr>().ok()
}
impl Scalar for Ipv6 {
    fn name(&self) -> &str { "ipv6" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an IPv6 address")?;
        let a = parse_ipv6(w).ok_or_else(|| format!("'{w}' is not an IPv6 address"))?;
        Ok((Value::Str(a.to_string()), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, "ipv6")?;
        parse_ipv6(s).map(|a| vec![a.to_string()]).ok_or_else(|| format!("'{s}' is not an IPv6 address"))
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^[0-9A-Fa-f:.]+$"), "IPv6 address") }
}

/// `addr/len` IPv6 prefix; the address part is canonicalised.
struct Ipv6Cidr;
fn parse_ipv6_cidr(w: &str) -> Option<String> {
    let (a, l) = w.split_once('/')?;
    let a = parse_ipv6(a)?;
    let l: u32 = l.parse().ok().filter(|l| *l <= 128)?;
    Some(format!("{a}/{l}"))
}
impl Scalar for Ipv6Cidr {
    fn name(&self) -> &str { "ipv6cidr" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an IPv6 prefix")?;
        Ok((Value::Str(parse_ipv6_cidr(w).ok_or_else(|| format!("'{w}' is not an IPv6 prefix (addr/len)"))?), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, "ipv6cidr")?;
        parse_ipv6_cidr(s).map(|c| vec![c]).ok_or_else(|| format!("'{s}' is not an IPv6 prefix (addr/len)"))
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^[0-9A-Fa-f:.]+/\d{1,3}$"), "IPv6 prefix, addr/len") }
}

/// BGP AS number: asplain or asdot on input, asplain on output.
struct Asn;
fn parse_asn(w: &str) -> Option<i64> {
    if let Some((h, l)) = w.split_once('.') {
        let h: i64 = h.parse().ok()?;
        let l: i64 = l.parse().ok()?;
        if h > 65535 || l > 65535 { return None; }
        return Some(h * 65536 + l);
    }
    let n: i64 = w.parse().ok()?;
    if (1..=4294967295).contains(&n) { Some(n) } else { None }
}
impl Scalar for Asn {
    fn name(&self) -> &str { "asn" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an AS number")?;
        Ok((Value::Int(parse_asn(w).ok_or_else(|| format!("'{w}' is not an AS number"))?), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        match v { Value::Int(i) if (1..=4294967295).contains(i) => Ok(vec![i.to_string()]), _ => Err(format!("{v:?} is not an AS number")) }
    }
    fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "integer", "minimum": 1, "maximum": 4294967295u64}) }
}

/// Free text to the end of the line, whitespace-normalized.
struct Phrase;
impl Scalar for Phrase {
    fn name(&self) -> &str { "phrase" }
    fn rest_of_line(&self) -> bool { true }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if t.is_empty() { return Err("expected text".into()); }
        Ok((Value::Str(t.join(" ")), t.len()))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        // One token: indentation renderers print it as-is, brace renderers quote it.
        Ok(vec![expect_str(v, "phrase")?.split_ascii_whitespace().collect::<Vec<_>>().join(" ")])
    }
    fn schema(&self) -> serde_json::Value { json_str(None, "free text") }
}

/// One or more words to the end of the line, as a list.
struct Names;
impl Scalar for Names {
    fn name(&self) -> &str { "names" }
    fn rest_of_line(&self) -> bool { true }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if t.is_empty() { return Err("expected one or more names".into()); }
        Ok((Value::List(t.iter().map(|w| Value::Str(w.to_string())).collect()), t.len()))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let l = v.as_list().ok_or_else(|| format!("expected a list of names, got {v:?}"))?;
        if l.is_empty() { return Err("names must not be empty".into()); }
        l.iter().map(|x| expect_str(x, "names").map(String::from)).collect()
    }
    fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "array", "items": {"type": "string"}, "minItems": 1}) }
}

/// One or more integers to the end of the line.
struct Ints;
impl Scalar for Ints {
    fn name(&self) -> &str { "ints" }
    fn rest_of_line(&self) -> bool { true }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if t.is_empty() { return Err("expected one or more integers".into()); }
        let vs = t.iter().map(|w| w.parse::<i64>().map(Value::Int).map_err(|_| format!("'{w}' is not an integer"))).collect::<Result<Vec<_>, _>>()?;
        Ok((Value::List(vs), t.len()))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let l = v.as_list().ok_or_else(|| format!("expected a list of integers, got {v:?}"))?;
        l.iter().map(|x| match x { Value::Int(i) => Ok(i.to_string()), _ => Err(format!("{x:?} is not an integer")) }).collect()
    }
    fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "array", "items": {"type": "integer"}, "minItems": 1}) }
}

/// Exactly two integers, e.g. `timers 10 30`, as a two-element list.
struct IntPair;
impl Scalar for IntPair {
    fn name(&self) -> &str { "intpair" }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if t.len() < 2 { return Err("expected two integers".into()); }
        let a: i64 = t[0].parse().map_err(|_| format!("'{}' is not an integer", t[0]))?;
        let b: i64 = t[1].parse().map_err(|_| format!("'{}' is not an integer", t[1]))?;
        Ok((Value::List(vec![Value::Int(a), Value::Int(b)]), 2))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        match v.as_list() {
            Some([Value::Int(a), Value::Int(b)]) => Ok(vec![a.to_string(), b.to_string()]),
            _ => Err(format!("expected [int, int], got {v:?}")),
        }
    }
    fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2}) }
}

/// A user-defined type: one token matching a regex (`type vrf = /[A-Z0-9_-]+/`).
pub struct RegexType { pub name: String, pub source: String, pub re: Regex }
impl Scalar for RegexType {
    fn name(&self) -> &str { &self.name }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, &self.name)?;
        if self.re.is_match(w) { Ok((Value::Str(w.to_string()), 1)) } else { Err(format!("'{w}' is not a valid {} (/{}/)", self.name, self.re.as_str())) }
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, &self.name)?;
        if self.re.is_match(s) { Ok(vec![s.to_string()]) } else { Err(format!("'{s}' is not a valid {}", self.name)) }
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(&format!("^{}$", self.source)), &self.name) }
}

/// One alternative of a union: a literal token or another type.
pub enum Alt {
    Lit(String),
    Type(ScalarRef),
}

/// A user-defined disjunction: `type action = "permit" | "deny"`,
/// `type prependItem = asn | "auto"`. Alternatives are tried in order.
pub struct UnionType { pub name: String, pub alts: Vec<Alt> }
impl UnionType {
    fn describe_alts(&self) -> String {
        self.alts.iter().map(|a| match a { Alt::Lit(l) => format!("\"{l}\""), Alt::Type(t) => t.name().to_string() }).collect::<Vec<_>>().join(" | ")
    }
}
impl Scalar for UnionType {
    fn name(&self) -> &str { &self.name }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, &self.name)?;
        for a in &self.alts {
            match a {
                Alt::Lit(l) if l == w => return Ok((Value::Str(w.to_string()), 1)),
                Alt::Lit(_) => {}
                Alt::Type(ty) => if let Ok(r) = ty.parse(t) { return Ok(r); },
            }
        }
        Err(format!("'{w}' is not a valid {} ({})", self.name, self.describe_alts()))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        for a in &self.alts {
            match a {
                Alt::Lit(l) => if v.as_str() == Some(l.as_str()) { return Ok(vec![l.clone()]); },
                Alt::Type(ty) => if let Ok(r) = ty.encode(v) { return Ok(r); },
            }
        }
        Err(format!("{v:?} is not a valid {} ({})", self.name, self.describe_alts()))
    }
    fn schema(&self) -> serde_json::Value {
        let lits: Vec<&str> = self.alts.iter().filter_map(|a| match a { Alt::Lit(l) => Some(l.as_str()), _ => None }).collect();
        let mut any: Vec<serde_json::Value> = self.alts.iter().filter_map(|a| match a { Alt::Type(t) => Some(t.schema()), _ => None }).collect();
        if !lits.is_empty() { any.push(serde_json::json!({"type": "string", "enum": lits})); }
        if any.len() == 1 { any.pop().unwrap() } else { serde_json::json!({"anyOf": any}) }
    }
}

/// `list(T)`: one or more `T` to the end of the line, as a list.
pub struct ListType { pub elem: ScalarRef }
impl Scalar for ListType {
    fn name(&self) -> &str { "list" }
    fn describe(&self) -> String { format!("list({})", self.elem.name()) }
    fn rest_of_line(&self) -> bool { true }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        if t.is_empty() { return Err(format!("expected one or more {}", self.elem.name())); }
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < t.len() {
            let (v, n) = self.elem.parse(&t[pos..])?;
            out.push(v);
            pos += n;
        }
        Ok((Value::List(out), pos))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let l = v.as_list().ok_or_else(|| format!("expected a list of {}, got {v:?}", self.elem.name()))?;
        if l.is_empty() { return Err(format!("list of {} must not be empty", self.elem.name())); }
        let mut out = Vec::new();
        for x in l { out.extend(self.elem.encode(x)?); }
        Ok(out)
    }
    fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "array", "items": self.elem.schema(), "minItems": 1}) }
}

// ---- catalog -------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Catalog { types: HashMap<String, ScalarRef> }

impl Catalog {
    /// Builtins, with conventions taken from the dialect's knobs (`cidr: masked | slash`).
    pub fn builtin(knobs: &HashMap<String, String>) -> Result<Catalog, String> {
        let masked_cidr = match knobs.get("cidr").map(String::as_str) {
            None | Some("slash") => false,
            Some("masked") => true,
            Some(other) => return Err(format!("dialect: cidr must be `slash` or `masked`, got `{other}`")),
        };
        let mut c = Catalog { types: HashMap::new() };
        c.add(Arc::new(Str));
        c.add(Arc::new(Int { min: i64::MIN, max: i64::MAX }));
        c.add(Arc::new(Ipv4));
        c.add(Arc::new(Cidr { masked: masked_cidr }));
        c.add(Arc::new(Ipv6));
        c.add(Arc::new(Ipv6Cidr));
        // Either family: handy for BGP neighbors and static routes.
        let ip: ScalarRef = Arc::new(UnionType { name: "ip".into(), alts: vec![Alt::Type(Arc::new(Ipv4)), Alt::Type(Arc::new(Ipv6))] });
        c.add(ip);
        c.add(Arc::new(UnionType { name: "prefix".into(), alts: vec![Alt::Type(Arc::new(Cidr { masked: masked_cidr })), Alt::Type(Arc::new(Ipv6Cidr))] }));
        c.add(Arc::new(Asn));
        c.add(Arc::new(Phrase));
        c.add(Arc::new(Names));
        c.add(Arc::new(Ints));
        c.add(Arc::new(IntPair));
        Ok(c)
    }
    pub fn add(&mut self, t: ScalarRef) { self.types.insert(t.name().to_string(), t); }
    pub fn get(&self, name: &str) -> Option<&ScalarRef> { self.types.get(name) }
    pub fn names(&self) -> Vec<&str> { let mut v: Vec<&str> = self.types.keys().map(String::as_str).collect(); v.sort(); v }

    /// `int(576..9216)` and `list(T)` are created on demand from their spec.
    pub fn resolve(&self, spec: &str) -> Option<ScalarRef> {
        if let Some(t) = self.get(spec) { return Some(t.clone()); }
        if let Some(inner) = spec.strip_prefix("int(").and_then(|s| s.strip_suffix(')')) {
            let (a, b) = inner.split_once("..")?;
            return Some(Arc::new(Int { min: a.trim().parse().ok()?, max: b.trim().parse().ok()? }));
        }
        if let Some(inner) = spec.strip_prefix("list(").and_then(|s| s.strip_suffix(')')) {
            let elem = self.resolve(inner.trim())?;
            if elem.rest_of_line() { return None; }
            return Some(Arc::new(ListType { elem }));
        }
        None
    }
}
