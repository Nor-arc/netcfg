//! JSON Schema generation from model declarations, so YAML/JSON data gets editor
//! completion and validation from the same source the parser uses.

use crate::engine::{Compiled, Engine};
use crate::model::Kind;
use serde_json::{json, Map, Value as J};

impl Engine {
    /// A self-contained JSON Schema (draft 2020-12) for `model`, with every referenced
    /// model under `$defs`.
    pub fn schema(&self, model: &str) -> crate::Result<J> {
        let root = self.model(model).ok_or_else(|| crate::Error(format!("unknown model `{model}`")))?;
        let mut defs = Map::new();
        let mut todo = vec![root];
        while let Some(m) = todo.pop() {
            if defs.contains_key(&m.name) { continue; }
            defs.insert(m.name.clone(), self.model_schema(m));
            for f in m.fields.iter().filter(|f| f.kind.is_nested()) {
                if let Some(sub) = self.model(&f.type_spec) { todo.push(sub); }
            }
        }
        Ok(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("#/$defs/{}", model),
            "$defs": defs,
        }))
    }

    fn model_schema(&self, m: &Compiled) -> J {
        let mut props = Map::new();
        let mut required = Vec::new();
        for (i, f) in m.fields.iter().enumerate() {
            let s = match f.kind {
                Kind::Flag => json!({"type": "boolean", "default": f.default.as_ref().map(|d| d[0] == "true").unwrap_or(false)}),
                Kind::Many => json!({"type": "array", "items": {"$ref": format!("#/$defs/{}", f.type_spec)}}),
                Kind::Single { .. } => json!({"$ref": format!("#/$defs/{}", f.type_spec)}),
                _ => {
                    let mut s = m.field_type(i).map(|t| t.schema()).unwrap_or(json!({}));
                    if let (Some(d), Some(o)) = (&f.default, s.as_object_mut()) {
                        o.insert("default".into(), J::String(d.join(" ")));
                    }
                    // With a negation word, `null` is the explicitly negated form (`no ip address`).
                    if f.kind == Kind::Opt && self.dialect.negation.is_some() {
                        s = json!({"anyOf": [s, {"type": "null", "description": "explicitly negated"}]});
                    }
                    s
                }
            };
            let s = match (&f.doc, s) {
                (Some(doc), J::Object(mut o)) => { o.insert("description".into(), J::String(doc.clone())); J::Object(o) }
                (_, s) => s,
            };
            props.insert(f.name.clone(), s);
            if matches!(f.kind, Kind::Key | Kind::Scalar | Kind::Single { required: true }) && f.default.is_none() { required.push(J::String(f.name.clone())); }
        }
        let mut out = json!({"type": "object", "properties": props, "required": required, "additionalProperties": false});
        if let Some(doc) = &m.doc { out["description"] = J::String(doc.clone()); }
        out
    }
}
