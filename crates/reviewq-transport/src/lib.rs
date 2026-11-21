//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    BridgeDescriptor, BrowserSessions, ControlRequest, ServiceEndpoint, ServiceIdentity,
    ServiceRuntime, StartupState, Store, WorkflowIntegration, router,
};
