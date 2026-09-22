//! Generate or check the committed note schema bundle.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&onejudge::sdk_schema::note_bundle())?
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/note.json");
    if std::env::args().nth(1).as_deref() == Some("--check") {
        onejudge::sdk_schema::check_note_bundle(&path)?;
    } else {
        print!("{generated}");
    }
    Ok(())
}
