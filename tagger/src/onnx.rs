//! The tagger run over a parse inside the engine: the parse's pieces, their evidence and tier 0's
//! labels, the ONNX model's heads over windows of pieces, and the labels decoded into units with
//! parents by the hierarchy rule. The model and its vocabularies come from `export_tagger.py`
//! (`tagger.onnx` beside `tagger.json`); the evidence is `rows::document`'s, so training and the
//! product read the same numbers.
use crate::labels::{self, Label, Piece, REGIONS};
use crate::rows;
use crate::state::{self, ExtractionPage, Gold, State};
use legal_structure_model::DocumentStructure;
use legal_pdf_core::model::Page;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// What the export wrote beside the model.
#[derive(Deserialize)]
pub struct Meta {
    pub features: Vec<String>,
    /// (name, label column, classes) per head, in the model's output order.
    pub heads: Vec<(String, usize, usize)>,
    pub regions: Vec<String>,
    pub labels: usize,
    pub text_chars: usize,
    pub window: usize,
    pub context: usize,
    /// Reader vocabularies: the parser's region type, note mode and numbering family, by name;
    /// an unseen value reads 0.
    pub vocabs: Vec<HashMap<String, i64>>,
    pub mean: Vec<f32>,
    pub std: Vec<f32>,
}

pub struct Tagger {
    session: Session,
    meta: Meta,
}

fn language_patterns() -> &'static (Regex, Regex) {
    static P: OnceLock<(Regex, Regex)> = OnceLock::new();
    P.get_or_init(|| (
        Regex::new(r"(?i)\b(le|la|les|des|du|une|est|sont|aux|dans|pour|qui|sur|par|au|ou|et|de)\b|[éèêàçùâîô]").unwrap(),
        Regex::new(r"(?i)\b(the|of|and|to|in|is|for|that|by|with|or|any|shall|under|be|on|a)\b").unwrap(),
    ))
}

/// From -1 (English function words) to 1 (French), 0 where neither shows: the trainer's `language`.
fn language(text: &str) -> f32 {
    let (french, english) = language_patterns();
    let (f, e) = (french.find_iter(text).count() as f32, english.find_iter(text).count() as f32);
    (f - e) / (f + e).max(1.0)
}

/// The trainer's `chars`: each of the first `n` characters as a code in 1..=2048, 0 padding.
fn chars(text: &str, n: usize) -> Vec<i64> {
    let mut codes: Vec<i64> = text.chars().take(n).map(|c| ((c as u32).min(0x2FFF) % 2048) as i64 + 1).collect();
    codes.resize(n, 0);
    codes
}

fn argmax(values: &[f32]) -> usize {
    values.iter().enumerate().fold((0, f32::NEG_INFINITY), |best, (k, &v)| if v > best.1 { (k, v) } else { best }).0
}

impl Tagger {
    /// The model under `dir` (`tagger.onnx`, `tagger.json`), run with `threads` intra-op threads.
    pub fn load(dir: &Path, threads: usize) -> Result<Self, String> {
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(dir.join("tagger.json")).map_err(|e| format!("tagger.json: {e}"))?)
            .map_err(|e| format!("tagger.json: {e}"))?;
        if meta.mean.len() != meta.features.len() || meta.vocabs.len() != 3 {
            return Err("tagger.json does not match the exported model".into());
        }
        let session = Session::builder()
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.with_intra_threads(threads.max(1)))
            .and_then(|b| b.with_inter_threads(1))
            .and_then(|b| b.commit_from_file(dir.join("tagger.onnx")))
            .map_err(|e| format!("tagger.onnx: {e}"))?;
        Ok(Self { session, meta })
    }

    /// The parse re-labelled by the model and decoded into units: regions, starts, markers, heading
    /// levels, with parents by the hierarchy rule. With `hybrid`, notes, note references and document
    /// starts stay tier 0's (their pairs are its own).
    pub fn tag(&mut self, pages: &[Page], structure: &DocumentStructure, source_sha256: &str, hybrid: bool) -> Result<Gold, String> {
        let boxes: Vec<ExtractionPage> = pages.iter().map(|p| ExtractionPage {
            width: p.width, height: p.height, lines: p.lines.iter().map(|l| (l.id.clone(), l.bbox)).collect(),
        }).collect();
        let state = state::parsed(structure, &boxes)?;
        let regions = labels::parsed_regions(&state);
        let (pieces, tier0) = labels::labels(&state, &|p| regions.of(p))?;
        let predicted = self.predict(pages, &state, &pieces, &tier0, hybrid)?;
        Ok(labels::decode(&state, &pieces, &predicted, source_sha256))
    }

    /// Each piece's labels from the model (tier 0's where the pages do not place the piece).
    pub fn predict(&mut self, pages: &[Page], state: &State, pieces: &[Piece], tier0: &[Label], hybrid: bool) -> Result<Vec<Label>, String> {
        let none: Vec<Option<Label>> = vec![None; pieces.len()];
        let row = rows::document(pages, state, pieces, tier0, &none);
        let ids = row["ids"].as_array().ok_or("rows without ids")?;
        let by_anchor: HashMap<String, usize> = pieces.iter().enumerate().map(|(k, p)| (labels::anchor(state, p), k)).collect();
        let kept: Vec<usize> = ids.iter().map(|id| by_anchor.get(id.as_str().unwrap_or_default()).copied().ok_or("row names an absent piece")).collect::<Result<_, _>>()?;
        let n = kept.len();
        let width = self.meta.features.len();
        let text_chars = self.meta.text_chars;
        let mut features = vec![0f32; n * (width + 1)];
        let mut text = vec![0i64; n * text_chars];
        let mut reader = vec![0i64; n * 3];
        let mut tier = vec![0i64; n * self.meta.labels];
        let as_f = |v: &Value| v.as_f64().unwrap_or(0.0) as f32;
        for (r, (values, tuple, codes)) in row["features"].as_array().unwrap().iter()
            .zip(row["text"].as_array().unwrap()).zip(row["tier0"].as_array().unwrap()).map(|((a, b), c)| (a, b, c)).enumerate()
        {
            let values = values.as_array().unwrap();
            for (f, value) in values.iter().enumerate().take(width) {
                features[r * (width + 1) + f] = (as_f(value) - self.meta.mean[f]) / self.meta.std[f];
            }
            let tuple = tuple.as_array().unwrap();
            let piece_text = tuple[0].as_str().unwrap_or_default();
            features[r * (width + 1) + width] = language(piece_text);
            text[r * text_chars..(r + 1) * text_chars].copy_from_slice(&chars(piece_text, text_chars));
            for (k, vocab) in self.meta.vocabs.iter().enumerate() {
                reader[r * 3 + k] = tuple.get(k + 1).and_then(Value::as_str).and_then(|v| vocab.get(v)).copied().unwrap_or(0);
            }
            for (k, code) in codes.as_array().unwrap().iter().enumerate().take(self.meta.labels) {
                tier[r * self.meta.labels + k] = code.as_i64().unwrap_or(0);
            }
        }
        // Windows of `window` pieces with `context` more on each side; each piece's heads come from the
        // window holding it in its core.
        let heads = self.meta.heads.clone();
        let mut classes: Vec<Vec<usize>> = vec![vec![0; heads.len()]; n];
        let (window, context) = (self.meta.window.max(1), self.meta.context);
        let mut core = 0;
        while core < n {
            let (lo, hi) = (core.saturating_sub(context), (core + window + context).min(n));
            let length = hi - lo;
            let f = TensorRef::from_array_view(([1usize, length, width + 1], &features[lo * (width + 1)..hi * (width + 1)])).map_err(|e| e.to_string())?;
            let t0 = TensorRef::from_array_view(([1usize, length, self.meta.labels], &tier[lo * self.meta.labels..hi * self.meta.labels])).map_err(|e| e.to_string())?;
            let tx = TensorRef::from_array_view(([1usize, length, text_chars], &text[lo * text_chars..hi * text_chars])).map_err(|e| e.to_string())?;
            let rd = TensorRef::from_array_view(([1usize, length, 3], &reader[lo * 3..hi * 3])).map_err(|e| e.to_string())?;
            let outputs = self.session.run(ort::inputs!["features" => f, "tier0" => t0, "text" => tx, "reader" => rd]).map_err(|e| format!("tagger run: {e}"))?;
            for (h, (name, _, count)) in heads.iter().enumerate() {
                let (_, logits) = outputs[name.as_str()].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
                for r in core..(core + window).min(n) {
                    let at = (r - lo) * count;
                    classes[r][h] = argmax(&logits[at..at + count]);
                }
            }
            core += window;
        }
        // The trainer's reading of the heads: no start outside a region, a level on heading starts
        // only, a marker on starts only, a resumed piece resumes the latest earlier unit of its region.
        let heading = REGIONS.iter().position(|r| *r == "heading").unwrap_or(3);
        let note = REGIONS.iter().position(|r| *r == "note").unwrap_or(5);
        let column = |name: &str| heads.iter().position(|(h, _, _)| h == name);
        let (c_region, c_start, c_document, c_reference, c_level, c_marker) = (
            column("region").ok_or("model without a region head")?, column("start").ok_or("model without a start head")?,
            column("document"), column("reference"), column("level"), column("marker"),
        );
        let mut result = tier0.to_vec();
        let mut latest: HashMap<usize, usize> = HashMap::new();
        for (r, &k) in kept.iter().enumerate() {
            let c = &classes[r];
            let region = c[c_region];
            let mut start = if region == 0 { 0 } else { c[c_start] };
            let mut label = Label {
                region,
                start,
                continues: None,
                level: if region == heading && start == 1 { c_level.map_or(0, |h| c[h]) } else { 0 },
                marker: start == 1 && c_marker.is_some_and(|h| c[h] == 1),
                document: c_document.is_some_and(|h| c[h] == 1),
                reference: c_reference.is_some_and(|h| c[h] == 1),
            };
            if hybrid {
                // Tier 0 keeps its notes (their pairs are its own), its references and its document starts.
                if tier0[k].region == note || region == note {
                    label = Label { document: label.document, reference: label.reference, ..tier0[k].clone() };
                    start = label.start;
                }
                label.reference = tier0[k].reference;
                label.document = tier0[k].document;
            }
            if start == 2 {
                match latest.get(&label.region) {
                    Some(&earlier) => label.continues = Some(labels::anchor(state, &pieces[earlier])),
                    None => label.start = 1,
                }
            }
            if label.start == 1 {
                latest.insert(label.region, k);
            }
            result[k] = label;
        }
        Ok(result)
    }
}
