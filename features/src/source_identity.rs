use serde::Serialize;

/// The exact lookup keys and authored forms of one source in a store's inventory.
#[derive(Serialize)]
pub struct AliasGroup {
    pub keys: Vec<String>,
    pub forms: Vec<String>,
    pub ambiguous: bool,
}

impl AliasGroup {
    pub fn own(key: &str) -> Self {
        Self {
            keys: if key.is_empty() {
                Vec::new()
            } else {
                vec![key.to_owned()]
            },
            forms: Vec::new(),
            ambiguous: false,
        }
    }
}

#[cfg(feature = "citator")]
pub fn citation_key_version() -> String {
    use sha2::{Digest, Sha256};
    let table = Sha256::digest(legal_structure::citations::aliases::COMPILED);
    format!(
        "{}:{}",
        legal_structure::citations::key::KEY_VERSION,
        table[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}
