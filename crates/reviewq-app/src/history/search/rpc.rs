use reviewq_domain::identity::NativeInputId;

use super::SearchHit;
use crate::history::{NativePosition, NativeRpcStamp, RpcContentRef};
use crate::reading::Literal;

pub struct RpcSearchQuery {
    pub literal: Literal,
    pub from: Option<(NativeRpcStamp, RpcSearchPoint)>,
    pub reverse: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcSearchPoint {
    pub page: NativePosition,
    pub entry: Option<NativeInputId>,
    pub part: u16,
    pub byte: u64,
}

pub struct RpcSearchPage {
    pub observation: NativeRpcStamp,
    pub hits: Vec<SearchHit<RpcContentRef, RpcSearchPoint>>,
    pub next: Option<RpcSearchPoint>,
    pub partial: bool,
}
