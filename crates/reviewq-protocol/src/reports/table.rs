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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TablePage {
    pub header: String,
    pub header_in_source: bool,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 16)))]
    pub widths: Vec<u8>,
    pub first_row: crate::SafeCountDto,
    pub row_count: crate::SafeCountDto,
    pub total_rows: crate::SafeCountDto,
    pub markdown: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportTableRange {
    pub byte_start: crate::SafeCountDto,
    pub byte_end: crate::SafeCountDto,
    pub scalar_start: crate::SafeCountDto,
    pub previous_byte: Option<crate::SafeCountDto>,
    pub next_byte: Option<crate::SafeCountDto>,
    pub content: ReportTableContent,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportTableContent {
    Table { page: TablePage },
    Markdown { markdown: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportTable {
    pub receipt_id: crate::ResourceIdDto,
    pub sha256: crate::reports::ReportHashDto,
    pub table_start: Option<crate::SafeCountDto>,
    pub table_end: Option<crate::SafeCountDto>,
    pub range: Option<ReportTableRange>,
}

pub type ReportTableResponse = crate::Success<ReportTable>;
