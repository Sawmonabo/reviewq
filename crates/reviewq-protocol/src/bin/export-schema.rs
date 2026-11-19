//! Export the registered public schemas for the contract generator.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write};

use reviewq_protocol::changes::{PatchDiscussionsResponse, PatchFileIndexResponse};
use reviewq_protocol::events::{InvalidateEvent, ResetEvent};
use reviewq_protocol::history::{
    HistoryContentResponse, HistoryNavigationResponse, HistoryPageResponse, HistoryPlanResponse,
};
use reviewq_protocol::queue::{QueuePageResponse, ReviewOverviewResponse, ReviewSelectionResponse};
use reviewq_protocol::{
    DeliveryReceiptResponse, EventCursorDto, Failure, RunDetailResponse, SendMessageRequest,
};
use reviewq_protocol::{
    Success, actions, decisions, followups, launch, notifications, questions, reports, settings,
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
    registry.request::<reviewq_protocol::settings::TestConnection>("TestConnection")?;
    registry.request::<reviewq_protocol::settings::CatalogRequest>("CatalogRequest")?;
    registry.request::<reviewq_protocol::settings::Disconnect>("DisconnectSettings")?;
    registry.response::<reviewq_protocol::settings::SettingsResponse>("SettingsResponse")?;
    registry.response::<reviewq_protocol::settings::VerifiedConnectionResponse>(
        "VerifiedConnectionResponse",
    )?;
    registry.response::<reviewq_protocol::settings::CatalogResponse>("CatalogResponse")?;
    registry.response::<QueuePageResponse>("QueuePageResponse")?;
    registry.response::<ReviewOverviewResponse>("ReviewOverviewResponse")?;
    registry.response::<ReviewSelectionResponse>("ReviewSelectionResponse")?;
    registry.response::<RunDetailResponse>("RunDetailResponse")?;
    registry.response::<reviewq_protocol::TerminalResponse>("TerminalResponse")?;
    registry.response::<PatchFileIndexResponse>("PatchFileIndexResponse")?;
    registry.response::<PatchDiscussionsResponse>("PatchDiscussionsResponse")?;
    registry.response::<HistoryPageResponse>("HistoryPageResponse")?;
    registry.response::<HistoryNavigationResponse>("HistoryNavigationResponse")?;
    registry.response::<HistoryPlanResponse>("HistoryPlanResponse")?;
    registry.response::<HistoryContentResponse>("HistoryContentResponse")?;
    registry.response::<DeliveryReceiptResponse>("DeliveryReceiptResponse")?;
    registry.response::<reviewq_protocol::queue::RefreshResponse>("RefreshResponse")?;
    registry.response::<launch::CatalogResponse>("RunCatalogResponse")?;
    registry.response::<launch::PreviewResponse>("RunPreviewResponse")?;
    registry.response::<launch::ReceiptResponse>("LaunchReceiptResponse")?;
    registry.response::<questions::FormsResponse>("QuestionFormsResponse")?;
    registry.response::<questions::AnswerResponse>("QuestionAnswerResponse")?;
    registry.response::<Success<decisions::DecisionResult>>("ReviewDecisionResponse")?;
    registry.response::<Success<notifications::NotificationPage>>("NotificationPageResponse")?;
    registry.response::<Success<notifications::StateResult>>("NotificationStateResponse")?;
    registry.response::<Success<followups::Enrollment>>("FollowupEnrollmentResponse")?;
    registry.response::<Success<followups::Preferences>>("FollowupPreferencesResponse")?;
    registry.response::<Success<followups::MonitorReceipt>>("FollowupMonitorResponse")?;
    registry.response::<Success<Option<actions::Action>>>("CurrentActionResponse")?;
    registry.response::<Success<reports::ReportView>>("ReportViewResponse")?;
    registry.response::<Success<reports::ReportHistory>>("ReportHistoryResponse")?;
    registry.response::<Success<reports::ReportSection>>("ReportSectionResponse")?;
    registry.response::<Success<reports::CandidateReport>>("ReportCandidateResponse")?;
    registry.response::<Success<reports::ReportContext>>("ReportContextResponse")?;
    registry.response::<Failure>("Failure")?;
    registry.event::<InvalidateEvent>("InvalidateEvent")?;
    registry.event::<ResetEvent>("ResetEvent")?;
    registry.request::<settings::SaveSettings>("SaveSettings")?;
    registry.request::<launch::CatalogRequest>("RunCatalogRequest")?;
    registry.request::<launch::PrepareRequest>("RunPrepareRequest")?;
    registry.request::<launch::StartRequest>("RunStartRequest")?;
    registry.request::<questions::Answer>("QuestionAnswerRequest")?;
    registry.request::<decisions::DecisionRequest>("ReviewDecisionRequest")?;
    registry.request::<decisions::CheckpointRequest>("ReviewCheckpointRequest")?;
    registry.request::<notifications::StateRequest>("NotificationStateRequest")?;
    registry.request::<followups::PreferencesRequest>("FollowupPreferencesRequest")?;
    registry.request::<followups::ConfigureRequest>("FollowupConfigureRequest")?;
    registry.request::<followups::MonitorRequest>("FollowupMonitorRequest")?;
    registry.request::<reports::FeedbackRequest>("ReportFeedbackRequest")?;

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
        let mut renames = BTreeMap::new();
        if let Some(Value::Object(definitions)) = root.remove("definitions") {
            loop {
                let mut changed = false;
                for (field, definition) in &definitions {
                    if renames.contains_key(field) {
                        continue;
                    }
                    if field == name
                        || self.definitions.get(field).is_some_and(|existing| {
                            let mut incoming = definition.clone();
                            rewrite_references(&mut incoming, &renames);
                            existing != &incoming
                        })
                    {
                        renames.insert(field.clone(), format!("{name}{field}"));
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            for (definition_name, mut definition) in definitions {
                rewrite_references(&mut definition, &renames);
                if let Some(owner) = renames.get(&definition_name) {
                    if let Some(object) = definition.as_object_mut() {
                        object.insert("title".into(), json!(owner));
                    }
                    self.insert_definition(owner.clone(), definition)?;
                } else {
                    self.insert_definition(definition_name, definition)?;
                }
            }
        }
        root.insert("title".into(), json!(name));
        let mut root = Value::Object(root);
        rewrite_references(&mut root, &renames);
        self.insert_definition(name.into(), root)
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

fn rewrite_references(schema: &mut Value, renames: &BTreeMap<String, String>) {
    match schema {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref")
                && let Some(prefix) = reference.strip_prefix("#/definitions/")
                && let Some(mapped) = renames.get(prefix)
            {
                *reference = format!("#/definitions/{mapped}");
            }
            for value in object.values_mut() {
                rewrite_references(value, renames);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_references(item, renames);
            }
        }
        _ => (),
    }
}
