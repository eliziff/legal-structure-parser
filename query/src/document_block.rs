use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Paragraph,
    Page,
    Section,
    Footnote,
    Table,
    Row,
    Cell,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentOrigin {
    Native,
    Heuristic,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum BlockFieldOrder {
    #[default]
    Projected,
    EndLast,
}

#[derive(Clone, Deserialize)]
pub struct DocumentBlock {
    pub kind: DocumentKind,
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub origin: DocumentOrigin,
    pub anchor: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(rename = "parentLabel")]
    pub parent_label: Option<String>,
    #[serde(rename = "rowSpan")]
    pub row_span: Option<usize>,
    #[serde(rename = "columnSpan")]
    pub column_span: Option<usize>,
    #[serde(skip)]
    pub(crate) field_order: BlockFieldOrder,
}

impl DocumentBlock {
    pub(crate) fn new(
        kind: DocumentKind,
        label: impl Into<String>,
        start: usize,
        end: usize,
        origin: DocumentOrigin,
    ) -> Self {
        Self {
            kind,
            label: label.into(),
            start,
            end,
            origin,
            anchor: None,
            aliases: Vec::new(),
            parent_label: None,
            row_span: None,
            column_span: None,
            field_order: BlockFieldOrder::Projected,
        }
    }

    fn fields(&self) -> usize {
        5 + usize::from(self.anchor.is_some())
            + usize::from(!self.aliases.is_empty())
            + usize::from(self.parent_label.is_some())
            + usize::from(self.row_span.is_some())
            + usize::from(self.column_span.is_some())
    }
}

impl Serialize for DocumentBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut row = serializer.serialize_struct("DocumentBlock", self.fields())?;
        row.serialize_field("kind", &self.kind)?;
        row.serialize_field("label", &self.label)?;
        row.serialize_field("start", &self.start)?;
        match self.field_order {
            BlockFieldOrder::Projected => {
                row.serialize_field("end", &self.end)?;
                row.serialize_field("origin", &self.origin)?;
                if !self.aliases.is_empty() {
                    row.serialize_field("aliases", &self.aliases)?;
                }
                if let Some(anchor) = &self.anchor {
                    row.serialize_field("anchor", anchor)?;
                }
                if let Some(span) = self.row_span {
                    row.serialize_field("rowSpan", &span)?;
                }
                if let Some(span) = self.column_span {
                    row.serialize_field("columnSpan", &span)?;
                }
            }
            BlockFieldOrder::EndLast => {
                if let Some(anchor) = &self.anchor {
                    row.serialize_field("anchor", anchor)?;
                }
                if !self.aliases.is_empty() {
                    row.serialize_field("aliases", &self.aliases)?;
                }
                if let Some(span) = self.row_span {
                    row.serialize_field("rowSpan", &span)?;
                }
                if let Some(span) = self.column_span {
                    row.serialize_field("columnSpan", &span)?;
                }
                row.serialize_field("origin", &self.origin)?;
                row.serialize_field("end", &self.end)?;
            }
        }
        if let Some(parent) = &self.parent_label {
            row.serialize_field("parentLabel", parent)?;
        }
        row.end()
    }
}
