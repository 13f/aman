use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JsonSchema(Value);

impl JsonSchema {
    #[must_use]
    pub fn empty() -> Self {
        Self(Value::Object(Map::new()))
    }

    #[must_use]
    pub const fn as_value(&self) -> &Value {
        &self.0
    }

    #[must_use]
    pub fn into_value(self) -> Value {
        self.0
    }
}

impl Default for JsonSchema {
    fn default() -> Self {
        Self::empty()
    }
}

impl From<Value> for JsonSchema {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::JsonSchema;
    use serde_json::json;

    #[test]
    fn empty_schema_is_empty_object() {
        let schema = JsonSchema::empty();
        assert_eq!(schema.as_value(), &json!({}));
    }

    #[test]
    fn default_schema_matches_empty() {
        assert_eq!(
            JsonSchema::default().as_value(),
            JsonSchema::empty().as_value()
        );
    }

    #[test]
    fn schema_round_trips_inner_value() {
        let original = json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"}
            }
        });

        let schema = JsonSchema::from(original.clone());
        assert_eq!(schema.as_value(), &original);
        assert_eq!(schema.into_value(), original);
    }

    #[test]
    fn schema_serializes_transparently() {
        let schema = JsonSchema::from(json!({"type": "string"}));
        let serialized = serde_json::to_value(schema).expect("serialize");
        assert_eq!(serialized, json!({"type": "string"}));
    }
}
