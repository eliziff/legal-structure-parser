use serde::Serialize;

/// The exact lookup keys and authored forms of one source in a store's inventory.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct AliasGroup {
    pub keys: Vec<String>,
    pub forms: Vec<String>,
    pub ambiguous: bool,
}

impl AliasGroup {
    pub fn own(key: &str) -> Self {
        Self { keys: (!key.is_empty()).then(|| key.to_owned()).into_iter().collect(), forms: Vec::new(), ambiguous: false }
    }
}

#[cfg(feature = "citator")]
pub fn citation_key_version() -> String {
    use sha2::{Digest, Sha256};
    let table = Sha256::digest(legal_structure::citations::aliases::COMPILED);
    let hex = table[..6].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("{}:{hex}", legal_structure::citations::key::KEY_VERSION)
}
