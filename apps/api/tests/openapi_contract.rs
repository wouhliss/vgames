//! A1-T03: the OpenAPI document generated from the handlers must match the contract
//! in `openapi/openapi.yaml`.
//!
//! Both documents are normalized per operation (`METHOD /path`) into: operationId,
//! public-or-authenticated, parameters `(in, name, required)`, request body (required,
//! content types, schema shape) and responses (status → content types → schema shape).
//! Schema shapes resolve `$ref`s and keep what clients depend on: types, property names,
//! required sets, array items, maps, multi-value enums and `oneOf` variants. They ignore
//! documentation (descriptions, formats, patterns, ranges), `null` in optional fields, and
//! single-value enums/consts (a Rust `String` field may implement `const: vgames.sig/1`).
//!
//! Contract operations that are not implemented yet must be listed in
//! `tests/openapi_unimplemented.txt`. The list must only shrink (A1-T17 empties it).
//! `cargo xtask openapi check` runs this test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

fn contract() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi/openapi.yaml");
    let text = std::fs::read_to_string(path).expect("read openapi/openapi.yaml");
    serde_yaml_ng::from_str(&text).expect("parse openapi.yaml")
}

fn generated() -> Value {
    serde_json::to_value(vgames_api::http::openapi()).expect("serialize generated document")
}

fn unimplemented_allowlist() -> BTreeSet<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/openapi_unimplemented.txt"
    );
    std::fs::read_to_string(path)
        .expect("read openapi_unimplemented.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------------------

fn resolve<'a>(doc: &'a Value, v: &'a Value) -> &'a Value {
    let mut cur = v;
    for _ in 0..16 {
        let Some(r) = cur.get("$ref").and_then(Value::as_str) else {
            return cur;
        };
        let pointer = r.strip_prefix('#').unwrap_or(r);
        cur = doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("dangling $ref {r}"));
    }
    cur
}

fn types_of(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn is_null_schema(doc: &Value, s: &Value) -> bool {
    let s = resolve(doc, s);
    types_of(s) == ["null"] || s.get("enum") == Some(&json!([null]))
}

fn shape(doc: &Value, schema: &Value, depth: usize) -> Value {
    if depth > 24 {
        return json!("<recursive>");
    }
    let schema = resolve(doc, schema);

    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        let parts: Vec<Value> = all.iter().map(|s| shape(doc, s, depth + 1)).collect();
        return merge_all_of(parts);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(list) = schema.get(key).and_then(Value::as_array) {
            let mut variants: Vec<Value> = list
                .iter()
                .filter(|s| !is_null_schema(doc, s))
                .map(|s| shape(doc, s, depth + 1))
                .collect();
            variants.sort_by_key(Value::to_string);
            variants.dedup();
            return if variants.len() == 1 {
                variants.remove(0)
            } else {
                json!({ "oneOf": variants })
            };
        }
    }

    let mut types: Vec<String> = types_of(schema)
        .into_iter()
        .filter(|t| t != "null")
        .collect();
    if types.is_empty()
        && (schema.get("properties").is_some() || schema.get("additionalProperties").is_some())
    {
        types.push("object".into());
    }
    if types.is_empty() && schema.get("items").is_some() {
        types.push("array".into());
    }
    if types.is_empty()
        && let Some(c) = schema.get("const")
    {
        types.push(json_type(c).into());
    }
    let ty = match types.as_slice() {
        [t] => t.clone(),
        [] => return json!("any"),
        many => return json!({ "types": many }),
    };
    match ty.as_str() {
        "object" => {
            let mut props = Map::new();
            if let Some(p) = schema.get("properties").and_then(Value::as_object) {
                for (k, v) in p {
                    props.insert(k.clone(), shape(doc, v, depth + 1));
                }
            }
            let mut required: Vec<String> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            required.sort();
            let mut out = json!({ "type": "object", "properties": props, "required": required });
            if let Some(ap) = schema.get("additionalProperties")
                && ap.is_object()
            {
                out["map_values"] = shape(doc, ap, depth + 1);
            }
            out
        }
        "array" => {
            json!({ "type": "array", "items": schema.get("items").map_or(json!("any"), |i| shape(doc, i, depth + 1)) })
        }
        other => match schema.get("enum").and_then(Value::as_array) {
            Some(values) => {
                let mut vals: Vec<String> = values
                    .iter()
                    .filter(|v| !v.is_null())
                    .map(Value::to_string)
                    .collect();
                vals.sort();
                vals.dedup();
                if vals.len() > 1 {
                    json!({ "type": other, "enum": vals })
                } else {
                    json!(other)
                }
            }
            None => json!(other),
        },
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

/// `allOf` of objects merges into one object; a single non-trivial part stands for itself.
fn merge_all_of(parts: Vec<Value>) -> Value {
    let meaningful: Vec<Value> = parts.into_iter().filter(|p| p != "any").collect();
    if meaningful.len() == 1 {
        return meaningful.into_iter().next().unwrap_or(json!("any"));
    }
    if meaningful
        .iter()
        .all(|p| p.get("type") == Some(&json!("object")))
    {
        let mut props = Map::new();
        let mut required = BTreeSet::new();
        for p in &meaningful {
            if let Some(m) = p["properties"].as_object() {
                props.extend(m.clone());
            }
            if let Some(r) = p["required"].as_array() {
                required.extend(r.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
        return json!({ "type": "object", "properties": props, "required": required.into_iter().collect::<Vec<_>>() });
    }
    json!({ "allOf": meaningful })
}

fn content_shapes(doc: &Value, content: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(c) = content.and_then(Value::as_object) {
        for (ct, media) in c {
            let s = media
                .get("schema")
                .map_or(json!("any"), |s| shape(doc, s, 0));
            out.insert(ct.clone(), s);
        }
    }
    Value::Object(out)
}

fn security_kind(doc: &Value, op: &Value) -> String {
    let reqs = op.get("security").or_else(|| doc.get("security"));
    match reqs.and_then(Value::as_array) {
        None => "public".into(),
        Some(list)
            if list.is_empty()
                || list
                    .iter()
                    .all(|r| r.as_object().is_some_and(Map::is_empty)) =>
        {
            "public".into()
        }
        Some(list) => {
            let mut names: Vec<String> = list
                .iter()
                .filter_map(Value::as_object)
                .flat_map(|m| m.keys().cloned())
                .collect();
            names.sort();
            names.dedup();
            names.join("|")
        }
    }
}

fn normalize(doc: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return out;
    };
    for (path, item) in paths {
        let shared_params = item
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for method in METHODS {
            let Some(op) = item.get(method) else { continue };
            let mut params: Vec<Value> = shared_params
                .iter()
                .chain(
                    op.get("parameters")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .map(|p| {
                    let p = resolve(doc, p);
                    json!({
                        "in": p["in"],
                        "name": p["name"].as_str().map(str::to_ascii_lowercase),
                        "required": p.get("required").and_then(Value::as_bool).unwrap_or(false),
                        "schema": p.get("schema").map_or(json!("any"), |s| shape(doc, s, 0)),
                    })
                })
                .collect();
            params.sort_by_key(|p| format!("{}:{}", p["in"], p["name"]));

            let body = op.get("requestBody").map(|b| {
                let b = resolve(doc, b);
                json!({
                    "required": b.get("required").and_then(Value::as_bool).unwrap_or(false),
                    "content": content_shapes(doc, b.get("content")),
                })
            });

            let mut responses = Map::new();
            if let Some(r) = op.get("responses").and_then(Value::as_object) {
                for (status, resp) in r {
                    let resp = resolve(doc, resp);
                    responses.insert(status.clone(), content_shapes(doc, resp.get("content")));
                }
            }

            out.insert(
                format!("{} {}", method.to_uppercase(), path),
                json!({
                    "operationId": op.get("operationId"),
                    "security": security_kind(doc, op),
                    "parameters": params,
                    "requestBody": body,
                    "responses": responses,
                }),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------------------

fn diff(path: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
    match (want, got) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let p = format!("{path}/{k}");
                match (a.get(k), b.get(k)) {
                    (Some(x), Some(y)) => diff(&p, x, y, out),
                    (Some(x), None) => out.push(format!(
                        "  {p}: missing in generated (contract: {})",
                        short(x)
                    )),
                    (None, Some(y)) => {
                        out.push(format!("  {p}: not in contract (generated: {})", short(y)))
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}/{i}"), x, y, out);
            }
        }
        _ if want != got => out.push(format!(
            "  {path}: contract {} ≠ generated {}",
            short(want),
            short(got)
        )),
        _ => {}
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 160 {
        format!("{}…", &s[..160])
    } else {
        s
    }
}

#[test]
fn generated_document_matches_the_contract() {
    let contract = contract();
    let generated = generated();
    let want = normalize(&contract);
    let got = normalize(&generated);
    let allow = unimplemented_allowlist();

    let mut problems = Vec::new();
    for (op, spec) in &want {
        match got.get(op) {
            Some(g) => {
                if allow.contains(op) {
                    problems.push(format!(
                        "{op}: implemented now; remove it from tests/openapi_unimplemented.txt"
                    ));
                }
                let mut d = Vec::new();
                diff("", spec, g, &mut d);
                if !d.is_empty() {
                    problems.push(format!("{op}: differs from the contract\n{}", d.join("\n")));
                }
            }
            None if allow.contains(op) => {}
            None => problems.push(format!(
                "{op}: in the contract but not implemented (and not allowlisted)"
            )),
        }
    }
    for op in got.keys() {
        if !want.contains_key(op) {
            problems.push(format!(
                "{op}: implemented but not in openapi/openapi.yaml (write a contract: PR first)"
            ));
        }
    }
    for op in &allow {
        if !want.contains_key(op) {
            problems.push(format!("{op}: allowlisted but not in the contract"));
        }
    }
    assert!(
        problems.is_empty(),
        "OpenAPI drift ({} problem(s)):\n\n{}\n",
        problems.len(),
        problems.join("\n\n")
    );
}

#[test]
fn normalizer_ignores_documentation_but_not_structure() {
    let doc = json!({ "components": { "schemas": { "A": { "type": "object", "required": ["x"],
        "properties": { "x": { "type": "string", "format": "uuid", "description": "d" },
                        "y": { "type": ["integer", "null"] },
                        "k": { "const": "vgames.sig/1" },
                        "e": { "type": "string", "enum": ["a", "b"] } } } } } });
    let a = shape(&doc, &json!({ "$ref": "#/components/schemas/A" }), 0);
    let b = shape(
        &doc,
        &json!({ "type": "object", "required": ["x"], "properties": {
        "x": { "type": "string" }, "y": { "type": "integer" }, "k": { "type": "string" },
        "e": { "type": "string", "enum": ["b", "a"] } } }),
        0,
    );
    assert_eq!(a, b);
    let c = shape(
        &doc,
        &json!({ "type": "object", "required": [], "properties": {
        "x": { "type": "string" }, "y": { "type": "integer" }, "k": { "type": "string" },
        "e": { "type": "string", "enum": ["b", "a"] } } }),
        0,
    );
    assert_ne!(a, c, "required sets matter");
}
