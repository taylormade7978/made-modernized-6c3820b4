//! `content-validator` — validate the shipped content catalog against the
//! domain's *real* invariants before a build goes out.
//!
//! The catalog (card definitions today; expansions/bosses as they grow) ships as
//! JSON data files under `content/catalog`. CI runs this binary on every PR so a
//! catalog entry that would violate a domain invariant — an out-of-range Juice
//! cost, an unregistered effect script, a Legendary without its copy cap — fails
//! the build here rather than at runtime.
//!
//! Each card entry is fed through the very same [`CardDefinition::execute`] path
//! the authoritative server uses, so the validator can never drift from the
//! catalog rules: the single source of truth stays the aggregate. On top of the
//! per-entry domain check it also enforces one cross-file integrity invariant —
//! catalog identities are unique — which no single-aggregate check can see.
//!
//! Usage:
//!   content-validator [PATH]     (PATH defaults to `content/catalog`)
//!
//! Exit code is `0` when every entry is valid, `1` otherwise (with a per-entry
//! diagnostic on stderr).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

use domain::card_definition::{CardDefinition, DefineCardCmd};
use shared::Aggregate;

/// The default catalog root, relative to the repo/CWD.
const DEFAULT_ROOT: &str = "content/catalog";

/// One catalog file's shape. Every field is optional so a file may carry just
/// the entity kinds it defines; today only `cards` grows real validation, and
/// the array deserializes straight into the domain's `DefineCardCmd` (camelCase
/// schema), so the file schema *is* the command schema — no parallel model.
#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    cards: Vec<DefineCardCmd>,
}

fn main() -> ExitCode {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ROOT.to_string());
    let root = PathBuf::from(root);

    let files = match collect_json_files(&root) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("content-validator: cannot read '{}': {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    if files.is_empty() {
        eprintln!(
            "content-validator: no .json catalog files found under '{}'",
            root.display()
        );
        return ExitCode::FAILURE;
    }

    let mut errors: Vec<String> = Vec::new();
    let mut card_count = 0usize;
    // card_id -> the file it was first defined in, to catch cross-file collisions.
    let mut seen_cards: HashMap<String, PathBuf> = HashMap::new();

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) => {
                errors.push(format!("{}: unreadable: {e}", path.display()));
                continue;
            }
        };

        let file: CatalogFile = match serde_json::from_str(&raw) {
            Ok(file) => file,
            Err(e) => {
                errors.push(format!("{}: malformed catalog JSON: {e}", path.display()));
                continue;
            }
        };

        for card in file.cards {
            card_count += 1;
            let card_id = card.card_id.clone();

            // Cross-file integrity: a catalog identity is defined exactly once.
            if let Some(first) = seen_cards.get(&card_id) {
                errors.push(format!(
                    "{}: card '{card_id}' is already defined in {}",
                    path.display(),
                    first.display()
                ));
                continue;
            }
            seen_cards.insert(card_id.clone(), path.clone());

            // Per-entry: run the exact domain command path the server runs.
            let mut aggregate = CardDefinition::new(card_id.clone());
            if let Err(e) = aggregate.execute(card.into_command()) {
                errors.push(format!(
                    "{}: card '{card_id}' is invalid: {e:?}",
                    path.display()
                ));
            }
        }
    }

    if errors.is_empty() {
        println!(
            "content-validator: OK — {card_count} card(s) across {} file(s) satisfy every catalog invariant",
            files.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("content-validator: {} problem(s) found:", errors.len());
        for e in &errors {
            eprintln!("  - {e}");
        }
        ExitCode::FAILURE
    }
}

/// Gather every `*.json` file under `root` (recursively), sorted for stable,
/// reproducible output. A file path is accepted as-is so `content-validator
/// path/to/one.json` also works.
fn collect_json_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_file() {
        out.push(root.to_path_buf());
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(collect_json_files(&path)?);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
