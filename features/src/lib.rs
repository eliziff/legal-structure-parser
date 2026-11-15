//! What the products build on the structure engine, apart from it: the note-up graph built from
//! the local A2AJ store and the A2AJ documents it reads, quote location and verification, passage
//! links, journal-title search, a printed statute's currency, document outlines and
//! reading order. The engine's document readers (`legal-structure`) read none of this, so an edit
//! here leaves them, and every cache keyed on them, untouched.

#[cfg(feature = "a2aj-reader")]
pub mod a2aj;
pub mod http;
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "courtlistener")]
pub mod courtlistener;
#[cfg(feature = "foreign-case")]
pub mod foreign_case;
#[cfg(feature = "citator")]
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

#[cfg(all(feature = "citator", feature = "document-query"))]
pub use journal_titles::{
    journal_title_choice, journal_title_searches, JournalTitleHit, JournalTitleSearch,
};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use outline::{document_outline, text_outline, OutlineEntry, OutlineKind};
#[cfg(all(feature = "citator", feature = "document-query", feature = "canada"))]
pub use passage_links::{
    cited_passage_link, courtlistener_link, decisia_index, decisia_markup_pdf, decisia_pdf, host_passage_link, host_source_url, law_web_link, on_host, passage_link,
    quote_link, source_link, CourtListenerAnchor, CourtListenerCluster, CourtListenerOpinion, CourtListenerPage,
    LinkedSource, PassageLink, PassageLinkRequest, SourceLinkRequest, SourceLocator, VerifiedPdf,
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
#[cfg(feature = "citator")]
mod brief;
#[cfg(feature = "citator")]
pub use brief::{brief_reading, brief_review, brief_text, AttachedSource, BriefAuthority, BriefOccurrence, BriefReading,
    BriefReview, BriefText, BriefUnit, CitedCase, Locator, OccurrencePinpoint, OccurrenceReference, SourceDecision,
    SourceIdentity, StatuteCandidate, StatuteHit};
#[cfg(all(feature = "citator", feature = "canada", feature = "canlii"))]
pub use brief::{statute_name_key, statute_urls};
mod propositions;
pub use propositions::{footnote_propositions, FootnotePropositions, QuoteContext};
#[cfg(all(feature = "citator", feature = "quote-verification"))]
mod discrepancies;
#[cfg(all(feature = "citator", feature = "quote-verification"))]
pub use discrepancies::{correction_words, discrepancies, discrepancy_correction, discrepancy_occurrences, pinpoint_correction,
    CitedSource as DiscrepancySource, Correction as BriefCorrection, Finding as DiscrepancyFinding};
#[cfg(feature = "quote-verification")]
pub use propositions::{proposition_quotations, PropositionQuotation, QuotationCandidate, UnitCitation, UnitCitations};

mod source_identity;
#[cfg(feature = "citator")]
pub use source_identity::citation_key_version;
pub use source_identity::AliasGroup;
#[cfg(feature = "journal-store")]
pub mod journal_store;
#[cfg(feature = "a2aj-store")]
pub mod a2aj_store;
mod cited_answer;
pub mod search_query;
pub use cited_answer::*;
