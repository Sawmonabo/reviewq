use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PreparedTable {
    /// Half-open UTF-16 positions within the accompanying verified Markdown.
    pub start: u32,
    pub end: u32,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 16)))]
    pub columns: Vec<TableColumn>,
    #[cfg_attr(feature = "schema", schemars(length(min = 2, max = 129)))]
    pub rows: Vec<Vec<Vec<TableInline>>>,
    pub encoded_bytes: u32,
    pub nodes: u32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TableColumn {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TableInline {
    Text { start: u32, end: u32 },
    Code { start: u32, end: u32, text: String },
    Emphasis,
    Strong,
    Strike,
    Link { href: String, title: String },
    Close,
}
