// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA
        .get_or_init(|| serde_json::from_str(include_str!("../../../../api/schema.json")).unwrap())
}
pub fn validator(name: &str) -> Arc<jsonschema::Validator> {
    // Compile each immutable contract once. Recompiling the complete schema for
    // every poll can consume a real control lease even in paused-clock tests.
    static VALIDATORS: OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> =
        OnceLock::new();
    let mut validators = VALIDATORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    validators.entry(name.to_owned()).or_insert_with(|| {
        let root=schema();
        Arc::new(jsonschema::validator_for(&json!({"$schema":root["$schema"],"definitions":root["definitions"],"$ref":format!("#/definitions/{name}")})).unwrap())
    }).clone()
}
pub fn validate(name: &str, value: &Value) {
    let v = validator(name);
    let errors = v
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "{name}: {errors:?}; payload: {value}");
}
