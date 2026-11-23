//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    BridgeDescriptor, BrowserDesktop, BrowserOffer, ControlRequest, ServiceEndpoint,
    ServiceIdentity, ServiceRuntime, StartupState, Store, WorkflowIntegration, router,
};
