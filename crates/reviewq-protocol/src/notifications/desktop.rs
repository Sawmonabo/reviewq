//! Ephemeral frames for an authenticated browser desktop notification channel.

use serde::{Deserialize, Serialize};

use crate::ResourceIdDto;

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BrowserFrame {
    Hello { csrf_token: String },
    Accept { notification_id: ResourceIdDto },
    Decline { notification_id: ResourceIdDto },
    Displayed { notification_id: ResourceIdDto },
    Failed { notification_id: ResourceIdDto },
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DesktopFrame<'a> {
    Ready,
    Offer {
        notification_id: &'a str,
        title: &'static str,
        body: &'a str,
        event_count: u16,
    },
    Display {
        notification_id: &'a str,
    },
    Cancel {
        notification_id: &'a str,
    },
}
