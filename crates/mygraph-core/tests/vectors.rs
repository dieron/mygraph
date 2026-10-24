//! Runs the language-neutral vectors in `conformance/vectors/` (format: conformance/README.md).

use mygraph_core::Reason;
use mygraph_core::ids::{self, ObjectId, ObjectKind};
use mygraph_core::name_key::name_key;
use mygraph_core::values::{self, Parsed, Value};
use mygraph_core::vocab::{self, VocabKind};
use serde_json::{Value as Json, json};
use std::path::PathBuf;

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors")
}

fn repeat(spec: &Json) -> String {
    let prefix = spec.get("prefix").and_then(Json::as_str).unwrap_or("");
    let unit = spec["unit"].as_str().unwrap();
    let count = spec["count"].as_u64().unwrap() as usize;
    format!("{prefix}{}", unit.repeat(count))
}

fn input_str(case: &Json) -> String {
    match case.get("input_repeat") {
        Some(r) => repeat(r),
        None => case["input"].as_str().unwrap().to_owned(),
    }
}

fn expected(case: &Json) -> Json {
    if let Some(r) = case.get("output_repeat") {
        return Json::String(repeat(r));
    }
    let mut out = case["output"].clone();
    if let Some(obj) = out.as_object_mut() {
        if let Some(r) = obj.remove("value_repeat") {
            obj.insert("value".into(), Json::String(repeat(&r)));
        }
        if let Some(r) = obj.remove("value_list_repeat") {
            let n = r["count"].as_u64().unwrap() as usize;
            obj.insert("value".into(), Json::Array(vec![r["element"].clone(); n]));
        }
    }
    out
}

fn id_json(id: ObjectId) -> Json {
    json!({ "kind": id.kind.name(), "number": id.number })
}

fn parsed_json(p: Parsed) -> Json {
    match p {
        Parsed::Remove => json!({ "remove": true }),
        Parsed::Value(v) => {
            let mut out = json!({ "kind": v.kind_name(), "value": v.to_json() });
            if let Value::Date(d) = v {
                let p = format!("{:?}", d.precision()).to_lowercase();
                out["precision"] = Json::String(p);
            }
            out
        }
    }
}

fn pair(case: &Json) -> (Value, Value) {
    let Json::Array(items) =
        serde_json::from_str::<Json>(case["input_json"].as_str().unwrap()).unwrap()
    else {
        panic!("pair input must be an array")
    };
    let get = |j: &Json| match values::parse(j).unwrap() {
        Parsed::Value(v) => v,
        Parsed::Remove => panic!("pair element removes"),
    };
    (get(&items[0]), get(&items[1]))
}

fn run(function: &str, case: &Json) -> Result<Json, Reason> {
    let s = || input_str(case);
    Ok(match function {
        "encode_id" => {
            let kind = ObjectKind::from_name(case["input"]["kind"].as_str().unwrap()).unwrap();
            Json::String(ObjectId::new(kind, case["input"]["number"].as_u64().unwrap()).encode())
        }
        "decode_id" => id_json(ObjectId::decode(&s())?),
        "decode_id_of_kind" => {
            let kind =
                ObjectKind::from_name(case["input"]["expect_kind"].as_str().unwrap()).unwrap();
            id_json(ObjectId::decode_of_kind(
                case["input"]["id"].as_str().unwrap(),
                kind,
            )?)
        }
        "validate_key" => Json::String(ids::validate_key(&s())?),
        "validate_batch_ref" => Json::String(ids::validate_batch_ref(&s())?.to_owned()),
        "validate_graph_name" => Json::String(ids::validate_graph_name(&s())?.to_owned()),
        "validate_actor_name" => Json::String(ids::validate_actor_name(&s())?.to_owned()),
        "name_key" => Json::String(name_key(&s())),
        "canonical_node_type" => Json::String(vocab::canonicalize(VocabKind::NodeType, &s())?),
        "canonical_link_type" => Json::String(vocab::canonicalize(VocabKind::LinkType, &s())?),
        "canonical_prop_key" => Json::String(vocab::canonicalize(VocabKind::PropKey, &s())?),
        "validate_vocabulary_verbatim" => Json::String(vocab::validate_verbatim(
            case["input"]["value"].as_str().unwrap(),
        )?),
        "parse_value" => {
            if let Some(r) = case.get("input_repeat") {
                parsed_json(values::parse(&Json::String(repeat(r)))?)
            } else if let Some(r) = case.get("input_list_repeat") {
                let el = r["element_json"].as_str().unwrap();
                let text = format!(
                    "[{}]",
                    vec![el; r["count"].as_u64().unwrap() as usize].join(",")
                );
                parsed_json(values::parse_json_text(&text)?)
            } else {
                parsed_json(values::parse_json_text(
                    case["input_json"].as_str().unwrap(),
                )?)
            }
        }
        "compare_values" => {
            let (a, b) = pair(case);
            json!(values::compare(&a, &b) as i8)
        }
        "values_equal" => {
            let (a, b) = pair(case);
            json!(values::values_equal(&a, &b))
        }
        other => panic!("unknown vector function {other}"),
    })
}

/// Structural equality; numbers compare by value (vectors: floats numerically, ints exactly).
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (xs, ys) = (x.to_string(), y.to_string());
            match (xs.parse::<i128>(), ys.parse::<i128>()) {
                (Ok(p), Ok(q)) => p == q,
                _ => xs.parse::<f64>().ok() == ys.parse::<f64>().ok(),
            }
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

#[test]
fn shared_vectors() {
    let mut files: Vec<_> = std::fs::read_dir(vectors_dir())
        .expect("conformance/vectors exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let (mut total, mut failures) = (0, Vec::new());
    for file in &files {
        let doc: Json = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        let name = file.file_name().unwrap().to_string_lossy();
        for group in doc["groups"].as_array().unwrap() {
            let function = group["function"].as_str().unwrap();
            for case in group["cases"].as_array().unwrap() {
                total += 1;
                let got = run(function, case);
                let ok = match (&got, case.get("error")) {
                    (Err(r), Some(e)) => e.as_str() == Some(r.code()),
                    (Ok(v), None) => same(v, &expected(case)),
                    _ => false,
                };
                if !ok {
                    failures.push(format!("{name} {function} {case}: got {got:?}"));
                }
            }
        }
    }
    assert!(
        total > 300,
        "expected the full vector set, found {total} cases"
    );
    assert!(
        failures.is_empty(),
        "{} of {total} vector cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
