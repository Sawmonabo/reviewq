//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    BridgeDescriptor, ControlRequest, ServiceEndpoint, ServiceIdentity, ServiceRuntime, Store,
    WorkflowIntegration, router,
};
