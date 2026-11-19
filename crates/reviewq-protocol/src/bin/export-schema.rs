//! Export the registered public schemas for the contract generator.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write};

use reviewq_protocol::changes::{PatchDiscussionsResponse, PatchFileIndexResponse};
use reviewq_protocol::events::{InvalidateEvent, ResetEvent};
use reviewq_protocol::history::{HistoryContentResponse, HistoryPageResponse};
use reviewq_protocol::queue::{QueuePageResponse, ReviewOverviewResponse, ReviewSelectionResponse};
use reviewq_protocol::{
    DeliveryReceiptResponse, EventCursorDto, Failure, RunDetailResponse, SendMessageRequest,
};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Value, json};

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

#[derive(Default)]
struct Registry {
    definitions: BTreeMap<String, Value>,
    roots: BTreeMap<&'static str, BTreeMap<&'static str, String>>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = Registry::default();
    registry.request::<SendMessageRequest>("SendMessageRequest")?;
    registry.request::<EventCursorDto>("EventReplayCursor")?;
    registry.response::<QueuePageResponse>("QueuePageResponse")?;
    registry.response::<ReviewOverviewResponse>("ReviewOverviewResponse")?;
    registry.response::<ReviewSelectionResponse>("ReviewSelectionResponse")?;
    registry.response::<RunDetailResponse>("RunDetailResponse")?;
    registry.response::<reviewq_protocol::TerminalResponse>("TerminalResponse")?;
    registry.response::<PatchFileIndexResponse>("PatchFileIndexResponse")?;
    registry.response::<PatchDiscussionsResponse>("PatchDiscussionsResponse")?;
    registry.response::<HistoryPageResponse>("HistoryPageResponse")?;
    registry.response::<HistoryContentResponse>("HistoryContentResponse")?;
    registry.response::<DeliveryReceiptResponse>("DeliveryReceiptResponse")?;
    registry.response::<Failure>("Failure")?;
    registry.event::<InvalidateEvent>("InvalidateEvent")?;
    registry.event::<ResetEvent>("ResetEvent")?;

    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(
        &mut output,
        &json!({
            "$schema": DRAFT_07,
            "definitions": registry.definitions,
            "x-reviewq-roots": registry.roots,
        }),
    )?;
    writeln!(output)?;
    Ok(())
}

impl Registry {
    fn response<T: JsonSchema>(&mut self, name: &'static str) -> Result<(), Box<dyn Error>> {
        self.register::<T>("responses", name, SchemaSettings::draft07().for_serialize())
    }

    fn request<T: JsonSchema>(&mut self, name: &'static str) -> Result<(), Box<dyn Error>> {
        self.register::<T>(
            "requests",
            name,
            SchemaSettings::draft07().for_deserialize(),
        )
    }

    fn event<T: JsonSchema>(&mut self, name: &'static str) -> Result<(), Box<dyn Error>> {
        self.register::<T>("events", name, SchemaSettings::draft07().for_serialize())
    }

    fn register<T: JsonSchema>(
        &mut self,
        group: &'static str,
        name: &'static str,
        settings: SchemaSettings,
    ) -> Result<(), Box<dyn Error>> {
        let definition_path = format!("#/definitions/{name}");
        if self.roots.values().any(|roots| roots.contains_key(name)) {
            return Err(format!("duplicate contract root {name}").into());
        }
        self.roots
            .entry(group)
            .or_default()
            .insert(name, definition_path);

        let schema = settings.into_generator().into_root_schema_for::<T>();
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
