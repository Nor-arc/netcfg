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
    /// May match zero tokens (a union with an `""` alternative).
    fn allows_empty(&self) -> bool {
        false
    }
    /// Read a value from the start of `toks`, returning it and how many tokens were used.
    fn parse(&self, toks: &[&str]) -> Result<(Value, usize), String>;
    /// `parse`, but a type that would consume a variable number of tokens (a list) stops
    /// before any token in `stop`: the literals that follow it inside a struct type.
    fn parse_until(&self, toks: &[&str], stop: &[String]) -> Result<(Value, usize), String> {
        let _ = stop;
        self.parse(toks)
    }
    /// The literal tokens this type can match (a union's quoted alternatives, not `""`).
    fn literals(&self) -> Vec<&str> {
        Vec::new()
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String>;
    /// JSON Schema fragment for the value.
    fn schema(&self) -> serde_json::Value;
    fn describe(&self) -> String {
        self.name().to_string()
    }
    /// Extra detail for docs and skeletons: a union's alternatives, a regex.
    fn hint(&self) -> Option<String> {
        None
    }
    /// A struct type's named parts, in order.
    fn parts(&self) -> Vec<(&str, &ScalarRef)> {
        Vec::new()
    }
    /// A list type's element type.
    fn elem(&self) -> Option<&ScalarRef> {
        None
    }
}

pub type ScalarRef = Arc<dyn Scalar>;

fn one<'a>(toks: &[&'a str], what: &str) -> Result<&'a str, String> {
    toks.first().copied().ok_or_else(|| format!("expected {what}, found end of line"))
}
fn expect_str<'a>(v: &'a Value, ty: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("expected a string for {ty}, got {}", v.to_json()))
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
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let s = expect_str(v, "string")?;
        if s.is_empty() || s.contains(char::is_whitespace) { return Err(format!("'{s}' is not a single word")); }
        Ok(vec![s.to_string()])
    }
    fn schema(&self) -> serde_json::Value { json_str(Some(r"^\S+$"), "one word") }
}

struct Int { min: i64, max: i64 }
impl Scalar for Int {
    fn name(&self) -> &str { "int" }
    fn describe(&self) -> String {
        if self.min == i64::MIN && self.max == i64::MAX { "int".into() } else { format!("int({}..{})", self.min, self.max) }
    }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let w = one(t, "an integer")?;
        let i: i64 = w.parse().map_err(|_| format!("'{w}' is not an integer"))?;
        if i < self.min || i > self.max { return Err(format!("{i} is outside {}..{}", self.min, self.max)); }
        Ok((Value::Int(i), 1))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        match v { Value::Int(i) => Ok(vec![i.to_string()]), _ => Err(format!("expected an integer, got {}", v.to_json())) }
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
        match v { Value::Int(i) if (1..=4294967295).contains(i) => Ok(vec![i.to_string()]), _ => Err(format!("{} is not an AS number", v.to_json())) }
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

/// A user-defined type: one token matching a regex (`type vrf = /[A-Z0-9_-]+/`).
pub struct RegexType { pub name: String, pub source: String, pub re: Regex }
impl Scalar for RegexType {
    fn name(&self) -> &str { &self.name }
    fn hint(&self) -> Option<String> { Some(format!("/{}/", self.source)) }
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

/// One alternative of a union: a literal token (with the data value it stands for) or
/// another type.
pub enum Alt {
    Lit(String, Value),
    Type(ScalarRef),
}

/// A user-defined disjunction: `type action = "permit" | "deny"`,
/// `type prependItem = asn | "auto"`, `type state = "up" -> true | "down" -> false`.
/// Alternatives are tried in order; a literal parses to its mapped value (itself by default).
pub struct UnionType { pub name: String, pub alts: Vec<Alt> }
impl UnionType {
    fn describe_alts(&self) -> String {
        self.alts.iter().map(|a| match a {
            Alt::Lit(l, Value::Str(v)) if v == l => format!("\"{l}\""),
            Alt::Lit(l, v) => format!("\"{l}\" -> {}", v.to_json()),
            Alt::Type(t) => t.describe(),
        }).collect::<Vec<_>>().join(" | ")
    }
}
impl Scalar for UnionType {
    fn name(&self) -> &str { &self.name }
    /// `""` as an alternative means "nothing". On a template line such a placeholder must be
    /// last (only the end of the line is unambiguous); inside a struct type it may sit anywhere.
    fn allows_empty(&self) -> bool { self.alts.iter().any(|a| matches!(a, Alt::Lit(l, _) if l.is_empty())) }
    fn rest_of_line(&self) -> bool { self.allows_empty() }
    fn hint(&self) -> Option<String> { Some(self.describe_alts()) }
    fn literals(&self) -> Vec<&str> {
        self.alts.iter().filter_map(|a| match a { Alt::Lit(l, _) if !l.is_empty() => Some(l.as_str()), _ => None }).collect()
    }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        for a in &self.alts {
            match a {
                Alt::Lit(l, v) if l.is_empty() => return Ok((v.clone(), 0)),
                Alt::Lit(l, v) => if t.first() == Some(&l.as_str()) { return Ok((v.clone(), 1)); },
                Alt::Type(ty) => if !t.is_empty() { if let Ok(r) = ty.parse(t) { return Ok(r); } },
            }
        }
        match t.first() {
            Some(w) => Err(format!("'{w}' is not a valid {} ({})", self.name, self.describe_alts())),
            None => Err(format!("expected {} ({}), found end of line", self.name, self.describe_alts())),
        }
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        for a in &self.alts {
            match a {
                Alt::Lit(l, lv) => if lv == v { return Ok(if l.is_empty() { Vec::new() } else { vec![l.clone()] }); },
                Alt::Type(ty) => if let Ok(r) = ty.encode(v) { return Ok(r); },
            }
        }
        Err(format!("{} is not a valid {} ({})", v.to_json(), self.name, self.describe_alts()))
    }
    fn schema(&self) -> serde_json::Value {
        let lits: Vec<serde_json::Value> = self.alts.iter().filter_map(|a| match a { Alt::Lit(_, v) => Some(v.to_json()), _ => None }).collect();
        let mut any: Vec<serde_json::Value> = self.alts.iter().filter_map(|a| match a { Alt::Type(t) => Some(t.schema()), _ => None }).collect();
        if !lits.is_empty() {
            if lits.iter().all(|l| l.is_string()) { any.push(serde_json::json!({"type": "string", "enum": lits})); }
            else { any.push(serde_json::json!({"enum": lits})); }
        }
        if any.len() == 1 { any.pop().unwrap() } else { serde_json::json!({"anyOf": any}) }
    }
}

/// One token of a struct type: a literal or a named sub-field.
pub enum SToken {
    Lit(String),
    Field { name: String, ty: ScalarRef },
}

/// A structured value declared in the template file:
/// `type maxRoutes = {{ limit: int }} {{ action: "warning-only" | "" }}`.
/// The value is a record; sub-fields whose type allows "nothing" are omitted when absent.
pub struct StructType {
    pub name: String,
    pub toks: Vec<SToken>,
    /// Per token, the literals that may follow it (see `new`).
    stops: Vec<Vec<String>>,
}
impl StructType {
    pub fn new(name: String, toks: Vec<SToken>) -> StructType {
        // Literals that may come after token `i`: literal tokens, and the literals of
        // placeholders up to and including the first one that cannot match nothing.
        let stops = (0..toks.len()).map(|i| {
            let mut out = Vec::new();
            for tok in &toks[i + 1..] {
                match tok {
                    SToken::Lit(l) => { out.push(l.clone()); break; }
                    SToken::Field { ty, .. } => {
                        out.extend(ty.literals().into_iter().map(String::from));
                        if !ty.allows_empty() { break; }
                    }
                }
            }
            out
        }).collect();
        StructType { name, toks, stops }
    }
}
impl Scalar for StructType {
    fn name(&self) -> &str { &self.name }
    fn rest_of_line(&self) -> bool {
        matches!(self.toks.last(), Some(SToken::Field { ty, .. }) if ty.rest_of_line())
    }
    fn parts(&self) -> Vec<(&str, &ScalarRef)> {
        self.toks.iter().filter_map(|t| match t { SToken::Field { name, ty } => Some((name.as_str(), ty)), _ => None }).collect()
    }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        let mut rec = crate::value::Record::new();
        let mut pos = 0;
        for (i, tok) in self.toks.iter().enumerate() {
            match tok {
                SToken::Lit(l) => {
                    if t.get(pos) == Some(&l.as_str()) { pos += 1; }
                    else { return Err(format!("{}: expected `{l}`, found {}", self.name, t.get(pos).map(|w| format!("'{w}'")).unwrap_or_else(|| "end of line".into()))); }
                }
                SToken::Field { name, ty } => {
                    let (v, n) = ty.parse_until(&t[pos..], &self.stops[i]).map_err(|e| format!("{}.{name}: {e}", self.name))?;
                    pos += n;
                    if !(n == 0 && v.as_str() == Some("")) { rec.insert(name.clone(), v); }
                }
            }
        }
        Ok((Value::Record(rec), pos))
    }
    fn encode(&self, v: &Value) -> Result<Vec<String>, String> {
        let rec = v.as_record().ok_or_else(|| format!("expected an object for {}, got {}", v.to_json(), self.name))?;
        for k in rec.keys() {
            if !self.toks.iter().any(|t| matches!(t, SToken::Field { name, .. } if name == k)) { return Err(format!("{}: unknown field `{k}`", self.name)); }
        }
        let mut out = Vec::new();
        for tok in &self.toks {
            match tok {
                SToken::Lit(l) => out.push(l.clone()),
                SToken::Field { name, ty } => match rec.get(name) {
                    Some(x) => out.extend(ty.encode(x).map_err(|e| format!("{}.{name}: {e}", self.name))?),
                    None if ty.allows_empty() => {}
                    None => return Err(format!("{}: field `{name}` is missing", self.name)),
                },
            }
        }
        Ok(out)
    }
    fn schema(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for tok in &self.toks {
            if let SToken::Field { name, ty } = tok {
                props.insert(name.clone(), ty.schema());
                if !ty.allows_empty() { required.push(serde_json::Value::String(name.clone())); }
            }
        }
        serde_json::json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
    }
}

/// `list(T)`: one or more `T` to the end of the line, as a list.
pub struct ListType { pub elem: ScalarRef }
impl Scalar for ListType {
    fn name(&self) -> &str { "list" }
    fn describe(&self) -> String { format!("list({})", self.elem.describe()) }
    fn elem(&self) -> Option<&ScalarRef> { Some(&self.elem) }
    fn rest_of_line(&self) -> bool { true }
    fn parse(&self, t: &[&str]) -> Result<(Value, usize), String> {
        self.parse_until(t, &[])
    }
    /// Stops before a stop word; a token that is both an element and a stop word is taken as
    /// the stop word.
    fn parse_until(&self, t: &[&str], stop: &[String]) -> Result<(Value, usize), String> {
        let end = t.iter().position(|w| stop.iter().any(|s| s == w)).unwrap_or(t.len());
        let t = &t[..end];
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
        let l = v.as_list().ok_or_else(|| format!("expected a list of {}, got {}", v.to_json(), self.elem.name()))?;
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
