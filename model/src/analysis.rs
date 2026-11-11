
/// The origin of the notes the pairer writes.
pub const NOTE_PAIRING_ORIGIN: &str = "legal-structure.note-pairing";

/// A laid-out document's notes as the pairer writes them: each a Footnote (or Endnote) node placed in the reader's text,
/// its body its ranges, its label its marker range, its calls; and the diagnostics of what it could not pair or place.
pub struct NotePairing {
    pub notes: Vec<crate::StructureNode>,
    pub diagnostics: Vec<legal_layout::Diagnostic>,
}
