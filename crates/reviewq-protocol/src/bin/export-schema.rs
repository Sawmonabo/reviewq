//! Export the registered public schemas for the contract generator.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write};

use reviewq_protocol::{DeliveryReceiptResponse, Failure};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Value, json};

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

#[derive(Default)]
struct Registry {
    definitions: BTreeMap<String, Value>,
    responses: BTreeMap<&'static str, String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = Registry::default();
    registry.response::<DeliveryReceiptResponse>("DeliveryReceiptResponse")?;
    registry.response::<Failure>("Failure")?;

    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(
        &mut output,
        &json!({
            "$schema": DRAFT_07,
            "definitions": registry.definitions,
            "x-reviewq-responses": registry.responses,
        }),
    )?;
    writeln!(output)?;
    Ok(())
}

impl Registry {
    fn response<T: JsonSchema>(&mut self, name: &'static str) -> Result<(), Box<dyn Error>> {
        let definition_path = format!("#/definitions/{name}");
        if self.responses.insert(name, definition_path).is_some() {
            return Err(format!("duplicate response name {name}").into());
        }

        let schema = SchemaSettings::draft07()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<T>();
        let Value::Object(mut root) = schema.to_value() else {
            return Err(format!("expected object schema for {name}").into());
        };
        root.remove("$schema");
        if let Some(Value::Object(definitions)) = root.remove("definitions") {
            for (definition_name, definition) in definitions {
                self.insert_definition(definition_name, definition)?;
            }
        }
        root.insert("title".into(), json!(name));
        self.insert_definition(name.into(), Value::Object(root))
    }

    fn insert_definition(&mut self, name: String, schema: Value) -> Result<(), Box<dyn Error>> {
        if let Some(existing) = self.definitions.get(&name) {
            if existing != &schema {
                return Err(format!("incompatible schemas named {name}").into());
            }
        } else {
            self.definitions.insert(name, schema);
        }
        Ok(())
    }
}
