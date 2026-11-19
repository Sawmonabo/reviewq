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
    features: BTreeMap<&'static str, &'static str>,
    roots: BTreeMap<&'static str, BTreeMap<&'static str, String>>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = Registry::default();
    registry.request::<EventCursorDto>("common", "EventReplayCursor")?;
    registry.response::<Failure>("common", "Failure")?;
    registry.event::<InvalidateEvent>("common", "InvalidateEvent")?;
    registry.event::<ResetEvent>("common", "ResetEvent")?;

    registry.request::<reviewq_protocol::settings::TestConnection>("settings", "TestConnection")?;
    registry.request::<reviewq_protocol::settings::CatalogRequest>("settings", "CatalogRequest")?;
    registry.request::<reviewq_protocol::settings::Disconnect>("settings", "DisconnectSettings")?;
    registry
        .response::<reviewq_protocol::settings::SettingsResponse>("settings", "SettingsResponse")?;
    registry.response::<reviewq_protocol::settings::VerifiedConnectionResponse>(
        "settings",
        "VerifiedConnectionResponse",
    )?;
    registry
        .response::<reviewq_protocol::settings::CatalogResponse>("settings", "CatalogResponse")?;
    registry.request::<settings::SaveSettings>("settings", "SaveSettings")?;
    registry.request::<settings::CheckoutSkillsRequest>("settings", "CheckoutSkillsRequest")?;
    registry.response::<settings::CheckoutSkillsResponse>("settings", "CheckoutSkillsResponse")?;

    registry.response::<QueuePageResponse>("queue", "QueuePageResponse")?;
    registry
        .request::<reviewq_protocol::queue::LoadSprintsRequest>("queue", "LoadSprintsRequest")?;
    registry
        .response::<reviewq_protocol::queue::LoadSprintsResponse>("queue", "LoadSprintsResponse")?;
    registry.response::<reviewq_protocol::queue::RunCardsResponse>("queue", "RunCardsResponse")?;
    registry.response::<reviewq_protocol::queue::ArchivePageResponse>(
        "archive",
        "ArchivePageResponse",
    )?;
    registry.response::<ReviewOverviewResponse>("queue", "ReviewOverviewResponse")?;
    registry.response::<ReviewSelectionResponse>("queue", "ReviewSelectionResponse")?;
    registry.response::<reviewq_protocol::queue::RefreshResponse>("queue", "RefreshResponse")?;
    registry
        .response::<Success<reviewq_protocol::queue::QueueSync>>("queue", "QueueSyncResponse")?;
    registry
        .request::<reviewq_protocol::queue::TrackReviewRequest>("queue", "TrackReviewRequest")?;
    registry
        .response::<reviewq_protocol::queue::TrackReviewResponse>("queue", "TrackReviewResponse")?;
    registry.response::<reviewq_protocol::queue::TicketDetailResponse>(
        "tickets",
        "TicketDetailResponse",
    )?;

    registry.response::<RunDetailResponse>("hosts", "RunDetailResponse")?;
    registry.response::<reviewq_protocol::TerminalResponse>("hosts", "TerminalResponse")?;
    registry.request::<reviewq_protocol::registration::RegisterSessionRequest>(
        "hosts",
        "RegisterSessionRequest",
    )?;
    registry.response::<reviewq_protocol::registration::RegisteredSessionResponse>(
        "hosts",
        "RegisteredSessionResponse",
    )?;

    registry.response::<PatchFileIndexResponse>("changes", "PatchFileIndexResponse")?;
    registry.response::<PatchDiscussionsResponse>("changes", "PatchDiscussionsResponse")?;
    registry.response::<reviewq_protocol::changes::ApprovalViewResponse>(
        "changes",
        "ApprovalViewResponse",
    )?;

    registry.response::<HistoryPageResponse>("history", "HistoryPageResponse")?;
    registry.response::<HistoryNavigationResponse>("history", "HistoryNavigationResponse")?;
    registry.response::<HistoryPlanResponse>("history", "HistoryPlanResponse")?;
    registry.response::<reviewq_protocol::history::HistoryPartPageResponse>(
        "history",
        "HistoryPartPageResponse",
    )?;
    registry.response::<HistoryContentResponse>("history", "HistoryContentResponse")?;

    registry.request::<SendMessageRequest>("delivery", "SendMessageRequest")?;
    registry.response::<DeliveryReceiptResponse>("delivery", "DeliveryReceiptResponse")?;
    registry.response::<Success<reviewq_protocol::MessageHistoryPage>>(
        "delivery",
        "MessageHistoryPageResponse",
    )?;

    registry.response::<launch::CatalogResponse>("launch", "RunCatalogResponse")?;
    registry.response::<launch::PreviewResponse>("launch", "RunPreviewResponse")?;
    registry.response::<launch::ReceiptResponse>("launch", "LaunchReceiptResponse")?;
    registry.request::<launch::CatalogRequest>("launch", "RunCatalogRequest")?;
    registry.request::<launch::PrepareRequest>("launch", "RunPrepareRequest")?;
    registry.request::<launch::StartRequest>("launch", "RunStartRequest")?;

    registry.response::<questions::FormsResponse>("questions", "QuestionFormsResponse")?;
    registry.response::<questions::AnswerResponse>("questions", "QuestionAnswerResponse")?;
    registry.request::<questions::Answer>("questions", "QuestionAnswerRequest")?;

    registry.response::<reviewq_protocol::recovery::RecoveryPreferencesResponse>(
        "recovery",
        "RecoveryPreferencesResponse",
    )?;
    registry.request::<reviewq_protocol::recovery::SavePreferences>(
        "recovery",
        "SaveRecoveryPreferencesRequest",
    )?;
    registry.response::<reviewq_protocol::recovery::RecoveryStateResponse>(
        "recovery",
        "RecoveryStateResponse",
    )?;
    registry.request::<reviewq_protocol::recovery::CancelRecovery>(
        "recovery",
        "CancelRecoveryRequest",
    )?;
    registry.response::<reviewq_protocol::recovery::CancelRecoveryResponse>(
        "recovery",
        "CancelRecoveryResponse",
    )?;
    registry
        .request::<reviewq_protocol::recovery::InterruptRequest>("recovery", "InterruptRequest")?;
    registry.response::<reviewq_protocol::recovery::InterruptResponse>(
        "recovery",
        "InterruptResponse",
    )?;

    registry
        .response::<Success<decisions::DecisionResult>>("decisions", "ReviewDecisionResponse")?;
    registry.request::<decisions::DecisionRequest>("decisions", "ReviewDecisionRequest")?;
    registry.request::<decisions::CheckpointRequest>("decisions", "ReviewCheckpointRequest")?;

    registry.response::<Success<notifications::NotificationPage>>(
        "notifications",
        "NotificationPageResponse",
    )?;
    registry.response::<Success<notifications::StateResult>>(
        "notifications",
        "NotificationStateResponse",
    )?;
    registry.request::<notifications::StateRequest>("notifications", "NotificationStateRequest")?;

    registry
        .response::<Success<followups::Enrollment>>("followups", "FollowupEnrollmentResponse")?;
    registry
        .response::<Success<followups::Preferences>>("followups", "FollowupPreferencesResponse")?;
    registry
        .response::<Success<followups::MonitorReceipt>>("followups", "FollowupMonitorResponse")?;
    registry.request::<followups::PreferencesRequest>("followups", "FollowupPreferencesRequest")?;
    registry.request::<followups::ConfigureRequest>("followups", "FollowupConfigureRequest")?;
    registry.request::<followups::MonitorRequest>("followups", "FollowupMonitorRequest")?;

    registry.response::<Success<Option<actions::Action>>>("actions", "CurrentActionResponse")?;

    registry.response::<Success<reports::ReportView>>("reports", "ReportViewResponse")?;
    registry.response::<Success<reports::ReportHistory>>("reports", "ReportHistoryResponse")?;
    registry.response::<Success<reports::ReportSection>>("reports", "ReportSectionResponse")?;
    registry.response::<Success<reports::CandidateReport>>("reports", "ReportCandidateResponse")?;
    registry.response::<Success<reports::ReportContext>>("reports", "ReportContextResponse")?;
    registry.request::<reports::FeedbackRequest>("reports", "ReportFeedbackRequest")?;
    registry.request::<reports::DispositionRequest>("reports", "ReportDispositionRequest")?;
    registry
        .response::<Success<reports::DispositionReceipt>>("reports", "ReportDispositionResponse")?;
    registry.request::<reports::ViewedRequest>("reports", "ReportViewedRequest")?;
    registry.response::<Success<reports::ViewedReceipt>>("reports", "ReportViewedResponse")?;

    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(
        &mut output,
        &json!({
            "$schema": DRAFT_07,
            "definitions": registry.definitions,
            "x-reviewq-roots": registry.roots,
            "x-reviewq-features": registry.features,
        }),
    )?;
    writeln!(output)?;
    Ok(())
}

impl Registry {
    fn response<T: JsonSchema>(
        &mut self,
        feature: &'static str,
        name: &'static str,
    ) -> Result<(), Box<dyn Error>> {
        self.register::<T>(
            "responses",
            feature,
            name,
            SchemaSettings::draft07().for_serialize(),
        )
    }

    fn request<T: JsonSchema>(
        &mut self,
        feature: &'static str,
        name: &'static str,
    ) -> Result<(), Box<dyn Error>> {
        self.register::<T>(
            "requests",
            feature,
            name,
            SchemaSettings::draft07().for_deserialize(),
        )
    }

    fn event<T: JsonSchema>(
        &mut self,
        feature: &'static str,
        name: &'static str,
    ) -> Result<(), Box<dyn Error>> {
        self.register::<T>(
            "events",
            feature,
            name,
            SchemaSettings::draft07().for_serialize(),
        )
    }

    fn register<T: JsonSchema>(
        &mut self,
        group: &'static str,
        feature: &'static str,
        name: &'static str,
        settings: SchemaSettings,
    ) -> Result<(), Box<dyn Error>> {
        let definition_path = format!("#/definitions/{name}");
        if self.roots.values().any(|roots| roots.contains_key(name)) {
            return Err(format!("duplicate contract root {name}").into());
        }
        self.features.insert(name, feature);
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
