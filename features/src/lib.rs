//! What the products build on the structure engine, apart from it: the note-up graph built from
//! the local A2AJ store and the A2AJ documents it reads, quote location and verification, passage
//! links, journal-title search, a printed statute's currency, compositions, document outlines and
//! reading order. The engine's document readers (`legal-structure`) read none of this, so an edit
//! here leaves them, and every cache keyed on them, untouched.

#[cfg(feature = "a2aj-reader")]
pub mod a2aj;
#[cfg(feature = "citator-graph")]
pub mod citator_graph;
mod composition;
mod composition_annotations;
mod composition_seed;
#[cfg(all(feature = "citator", feature = "document-query"))]
mod journal_titles;
#[cfg(all(feature = "provider-text", feature = "document-query"))]
mod outline;
#[cfg(all(feature = "citator", feature = "document-query", feature = "canada"))]
mod passage_links;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
mod printed_statute;
#[cfg(feature = "document-query")]
mod quote_location;
#[cfg(feature = "document-query")]
mod quote_verification;
mod reading_order;

pub use composition::*;
pub use composition_annotations::*;
pub use composition_seed::*;
#[cfg(all(feature = "citator", feature = "document-query"))]
pub use journal_titles::{journal_title_choice, journal_title_searches, JournalTitleHit, JournalTitleSearch};
#[cfg(all(feature = "provider-text", feature = "document-query"))]
pub use outline::{document_outline, OutlineEntry};
#[cfg(all(feature = "citator", feature = "document-query", feature = "canada"))]
pub use passage_links::{cited_passage_link, law_web_link, passage_link, source_link, PassageLink, PassageLinkRequest,
    SourceLinkRequest, SourceLocator, VerifiedPdf};
#[cfg(feature = "document-query")]
pub use quote_location::{
    corrected_quote, document_quote_blocks, editorial_quote, locate_quotes_in, locate_source_quotes, locate_source_quotes_kept, quote_text_comparison,
    verbatim_repair, LocatedQuote, QuoteBlockInput, QuoteLocator, QuoteSource, QuoteComparison, QuoteLocation, QuoteToLocate,
};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use printed_statute::{statute_currency, PrintedLine, PrintedStatute, ProvisionPlacement};
#[cfg(feature = "quote-verification")]
pub use quote_verification::*;
pub use reading_order::{document_reading_order, ReadingOrderUnit};

/// The digest of this crate's source, beside the engine's (`legal_structure::ENGINE_SOURCE_SHA256`).
pub const SOURCE_SHA256: &str = env!("LEGAL_STRUCTURE_FEATURES_SHA256");
