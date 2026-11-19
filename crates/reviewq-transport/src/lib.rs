//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    ControlRequest, ServiceEndpoint, ServiceIdentity, Store, WorkflowIntegration, router,
    router_with_workflow,
};
