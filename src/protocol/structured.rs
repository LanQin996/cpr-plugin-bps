//! 通过提示约束与终态校验实现结构化输出，不请求外部 schema

use super::{error, is_tool, text};
use gateway_plugin_sdk::PluginFault;
use serde_json::Value;

pub struct Structured {
    pub format: Value,
    validator: Option<jsonschema::Validator>,
}

impl Structured {
    pub fn new(value: &Value) -> Result<Option<Self>, PluginFault> {
        if value.is_null() {
            return Ok(None);
        }
        if !value.is_object() {
            return Err(error("Responses text must be an object."));
        }
        let format = &value["format"];
        if format.is_null() || format["type"] == "text" {
            return Ok(None);
        }
        let kind = text(&format["type"]);
        if !matches!(kind, "json_object" | "json_schema") {
            return Err(error(
                "BPS text.format supports text, json_object and json_schema.",
            ));
        }
        if format.as_object().is_none_or(|o| {
            o.keys().any(|key| {
                key != "type"
                    && (kind != "json_schema"
                        || !["name", "schema", "strict", "description"].contains(&key.as_str()))
            })
        }) {
            return Err(error("BPS text.format contains an unsupported field."));
        }
        let validator = if kind == "json_schema" {
            let name = text(&format["name"]);
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || format
                    .get("strict")
                    .is_some_and(|v| !v.is_null() && !v.is_boolean())
                || format
                    .get("description")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                || !format["schema"].is_object()
            {
                return Err(error("Invalid BPS JSON Schema output format."));
            }
            Some(compile(&format["schema"])?)
        } else {
            None
        };
        Ok(Some(Self {
            format: format.clone(),
            validator,
        }))
    }

    pub fn instructions(&self) -> String {
        format!(
            "The client requires a structured final answer. Return exactly one JSON value, without Markdown fences or surrounding prose. Tool calls and refusals remain separate protocol items. The gateway validates the final answer against this output format: {}",
            self.format
        )
    }

    pub fn validate(&self, response: &Value) -> Result<(), PluginFault> {
        let output = response["output"]
            .as_array()
            .ok_or_else(|| error("BPS structured response is missing output."))?;
        let mut answer = String::new();
        let mut refusal = false;
        let has_tool = output.iter().any(is_tool);
        for item in output {
            if item["type"] != "message" {
                continue;
            }
            for part in item["content"]
                .as_array()
                .ok_or_else(|| error("Invalid BPS structured message."))?
            {
                match text(&part["type"]) {
                    "output_text" => answer.push_str(
                        part["text"]
                            .as_str()
                            .ok_or_else(|| error("Invalid BPS structured text."))?,
                    ),
                    "refusal" => refusal = true,
                    _ => return Err(error("Unsupported BPS structured message content.")),
                }
            }
        }
        if has_tool || (refusal && answer.is_empty()) {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&answer)
            .map_err(|_| error("BPS structured output is not one valid JSON value."))?;
        if self
            .validator
            .as_ref()
            .is_some_and(|validator| !validator.is_valid(&value))
        {
            return Err(error(
                "BPS output does not satisfy the requested JSON Schema.",
            ));
        }
        Ok(())
    }
}

pub fn compile(schema: &Value) -> Result<jsonschema::Validator, PluginFault> {
    fn local(value: &Value, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        let Some(object) = value.as_object() else {
            return true;
        };
        object.iter().all(|(key, value)| match key.as_str() {
            "$ref" | "$dynamicRef" | "$recursiveRef" => value
                .as_str()
                .is_some_and(|reference| reference.starts_with('#')),
            "$defs" | "definitions" | "properties" | "patternProperties" | "dependentSchemas"
            | "dependencies" => value
                .as_object()
                .is_none_or(|schemas| schemas.values().all(|schema| local(schema, depth + 1))),
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => value
                .as_array()
                .is_none_or(|schemas| schemas.iter().all(|schema| local(schema, depth + 1))),
            "items" if value.is_array() => value
                .as_array()
                .unwrap()
                .iter()
                .all(|schema| local(schema, depth + 1)),
            "additionalProperties"
            | "unevaluatedProperties"
            | "propertyNames"
            | "items"
            | "contains"
            | "not"
            | "if"
            | "then"
            | "else"
            | "additionalItems"
            | "unevaluatedItems" => local(value, depth + 1),
            _ => true,
        })
    }
    if !local(schema, 0) {
        return Err(error(
            "Only in-document JSON Schema references are supported.",
        ));
    }
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|_| error("Invalid or unresolvable JSON Schema."))
}
