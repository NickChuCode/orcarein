//! JSON schema generation for `Intent`, used to expose each intent as a tool
//! to the LLM (one schema per intent call).

use serde_json::{json, Map, Value};

use super::{Intent, ParamType};

impl Intent {
    /// Return a JSON Schema `object` that describes the parameters the model
    /// must supply when invoking this intent.
    ///
    /// Schema shape:
    /// ```json
    /// {
    ///   "type": "object",
    ///   "properties": { "<name>": { <type-schema> }, … },
    ///   "required": ["<name>", …],
    ///   "additionalProperties": false
    /// }
    /// ```
    ///
    /// Type mappings:
    /// - `Int`   → `{"type":"integer"}` (with `minimum`/`maximum` when `Some`)
    /// - `Float` → `{"type":"number"}`  (with `minimum`/`maximum` when `Some`)
    /// - `String`→ `{"type":"string"}`  (with `"enum":[…]` when `enum_values` is `Some`)
    /// - `Bool`  → `{"type":"boolean"}`
    ///
    /// All params are required; no extra keys are accepted.
    pub fn json_schema(&self) -> Value {
        let mut properties: Map<String, Value> = Map::new();
        let mut required: Vec<Value> = Vec::new();

        for param in &self.params {
            let mut prop = match param.ty {
                ParamType::Int => {
                    let mut m = json!({ "type": "integer" });
                    if let Some(min) = param.min {
                        m["minimum"] = json!(min as i64);
                    }
                    if let Some(max) = param.max {
                        m["maximum"] = json!(max as i64);
                    }
                    m
                }
                ParamType::Float => {
                    let mut m = json!({ "type": "number" });
                    if let Some(min) = param.min {
                        m["minimum"] = json!(min);
                    }
                    if let Some(max) = param.max {
                        m["maximum"] = json!(max);
                    }
                    m
                }
                ParamType::String => {
                    let mut m = json!({ "type": "string" });
                    if let Some(ref ev) = param.enum_values {
                        let enum_arr: Vec<Value> = ev.iter().map(|s| json!(s)).collect();
                        m["enum"] = json!(enum_arr);
                    }
                    m
                }
                ParamType::Bool => {
                    json!({ "type": "boolean" })
                }
            };
            // Ensure prop is an object (it always will be given the arms above)
            let _ = prop.as_object_mut();
            properties.insert(param.name.clone(), prop);
            required.push(json!(param.name));
        }

        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::profile::{Backend, Intent, Param, ParamType, Risk};
    use serde_json::json;

    fn make_intent(params: Vec<Param>) -> Intent {
        Intent {
            name: "test_intent".to_string(),
            description: "A test intent".to_string(),
            risk: Risk::Safe,
            backend: Backend::Python {
                call: "dummy()".to_string(),
                returns: None,
            },
            params,
        }
    }

    /// Task spec: joint:int(min0,max5) + angle:int(min0,max180)
    #[test]
    fn schema_int_params_with_range() {
        let intent = make_intent(vec![
            Param {
                name: "joint".to_string(),
                ty: ParamType::Int,
                min: Some(0.0),
                max: Some(5.0),
                enum_values: None,
            },
            Param {
                name: "angle".to_string(),
                ty: ParamType::Int,
                min: Some(0.0),
                max: Some(180.0),
                enum_values: None,
            },
        ]);
        let schema = intent.json_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["joint"]["type"], "integer");
        assert_eq!(schema["properties"]["joint"]["minimum"], json!(0_i64));
        assert_eq!(schema["properties"]["joint"]["maximum"], json!(5_i64));
        assert_eq!(schema["properties"]["angle"]["type"], "integer");
        assert_eq!(schema["properties"]["angle"]["minimum"], json!(0_i64));
        assert_eq!(schema["properties"]["angle"]["maximum"], json!(180_i64));
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("joint")));
        assert!(required.contains(&json!("angle")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    /// String param with enum_values.
    #[test]
    fn schema_string_param_with_enum() {
        let intent = make_intent(vec![Param {
            name: "mode".to_string(),
            ty: ParamType::String,
            min: None,
            max: None,
            enum_values: Some(vec!["a".to_string(), "b".to_string()]),
        }]);
        let schema = intent.json_schema();
        assert_eq!(schema["properties"]["mode"]["type"], "string");
        assert_eq!(schema["properties"]["mode"]["enum"], json!(["a", "b"]));
    }

    /// Bool param.
    #[test]
    fn schema_bool_param() {
        let intent = make_intent(vec![Param {
            name: "enabled".to_string(),
            ty: ParamType::Bool,
            min: None,
            max: None,
            enum_values: None,
        }]);
        let schema = intent.json_schema();
        assert_eq!(schema["properties"]["enabled"]["type"], "boolean");
    }

    /// Float param with min/max.
    #[test]
    fn schema_float_param_with_range() {
        let intent = make_intent(vec![Param {
            name: "ratio".to_string(),
            ty: ParamType::Float,
            min: Some(0.0),
            max: Some(1.0),
            enum_values: None,
        }]);
        let schema = intent.json_schema();
        assert_eq!(schema["properties"]["ratio"]["type"], "number");
        assert_eq!(schema["properties"]["ratio"]["minimum"], json!(0.0));
        assert_eq!(schema["properties"]["ratio"]["maximum"], json!(1.0));
    }

    /// Intent with no params.
    #[test]
    fn schema_no_params() {
        let intent = make_intent(vec![]);
        let schema = intent.json_schema();
        assert_eq!(schema["type"], "object");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.is_empty());
        let required = schema["required"].as_array().unwrap();
        assert!(required.is_empty());
        assert_eq!(schema["additionalProperties"], json!(false));
    }
}
