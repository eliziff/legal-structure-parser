//! The structure tagger's view of a parse, shared by the product and the grader: the parser's lines
//! cut into pieces, each piece's evidence (features) and labels, tier 0's own labels, the gold's
//! labels carried onto a candidate, and decoding of labels back into units with parents by the
//! hierarchy rule. `state` holds the gold's projection and the parser's replay as one `State`.
pub mod alignment;
pub mod labels;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod rows;
pub mod state;
pub mod text_diff;
