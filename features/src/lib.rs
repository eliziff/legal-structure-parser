//! What the products build on the structure engine, apart from it: the note-up graph built from
//! the local A2AJ store and the A2AJ documents it reads, quote location and verification, passage
//! links, journal-title search, a printed statute's currency, document outlines and
//! reading order. The engine's document readers (`legal-structure`) read none of this, so an edit
//! here leaves them, and every cache keyed on them, untouched.

#[cfg(feature = "a2aj-reader")]
pub mod a2aj;
#[cfg(feature = "citator-graph")]
pub mod citator_graph;
#[cfg(all(feature = "citator", feature = "document-query", feature = "canada"))]
mod courtlistener_page;
#[cfg(all(feature = "citator", feature = "document-query"))]
mod journal_titles;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
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

#[cfg(all(feature = "citator", feature = "document-query"))]
pub use journal_titles::{
    journal_title_choice, journal_title_searches, JournalTitleHit, JournalTitleSearch,
};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use outline::{document_outline, text_outline, OutlineEntry};
#[cfg(all(feature = "citator", feature = "document-query", feature = "canada"))]
pub use passage_links::{
    cited_passage_link, courtlistener_link, law_web_link, passage_link, source_link,
    CourtListenerAnchor, CourtListenerCluster, CourtListenerOpinion, CourtListenerPage,
    PassageLink, PassageLinkRequest, SourceLinkRequest, VerifiedPdf,
};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use printed_statute::statute_currency;
#[cfg(feature = "document-query")]
pub use quote_location::{
    character_plan, corrected_quote, editorial_quote, highlight_runs, locate_document_quotes, quotation_diff, quote_occurrences,
    quote_text_comparison, sequence_opcodes, QuotationDiff,
    verbatim_repair, CitedPinpoint, LocatedQuote, QuoteBlockKind, QuoteBlockLabel, QuoteComparison,
    QuoteLocation, QuoteLocator, QuoteSource, QuoteToLocate,
};
#[cfg(feature = "quote-verification")]
pub use quote_verification::*;
pub use reading_order::{document_reading_order, ReadingOrderUnit};
mod propositions;
pub use propositions::{footnote_propositions, FootnotePropositions, PropositionUnit, QuoteContext};

/// The digest of this crate's source, beside the engine's (`legal_structure::ENGINE_SOURCE_SHA256`).
pub const SOURCE_SHA256: &str = env!("LEGAL_STRUCTURE_FEATURES_SHA256");

mod source_identity;
#[cfg(feature = "citator")]
pub use source_identity::citation_key_version;
pub use source_identity::AliasGroup;
#[cfg(feature = "a2aj-store")]
pub mod a2aj_store;
mod cited_answer;
pub mod search_query;
pub use cited_answer::*;
