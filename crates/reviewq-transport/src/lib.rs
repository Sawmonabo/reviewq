//! HTTP and MCP protocol adapters.

mod entry;

pub use entry::{
    ControlRequest, ServiceEndpoint, ServiceIdentity, Store, router, router_with_workflow,
};
