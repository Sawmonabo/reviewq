//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    BridgeDescriptor, BrowserDesktop, BrowserOffer, ControlRequest, ServiceEndpoint,
    ServiceIdentity, ServiceRuntime, StartupState, Store, WorkflowIntegration, field_error_text,
    ledger_refusal_text, router,
};
