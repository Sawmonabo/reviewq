//! HTTP and MCP protocol adapters.

mod entry;
mod start_door;

pub use entry::{
    BridgeDescriptor, BrowserDesktop, BrowserOffer, ControlRequest, ServiceEndpoint,
    ServiceIdentity, ServiceRuntime, StartupState, Store, WorkflowIntegration, field_error_text,
    ledger_refusal_text, router,
};
pub use start_door::start_door;
