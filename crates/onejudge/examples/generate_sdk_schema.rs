//! Print the single Rust-owned JSON Schema bundle consumed by language SDKs.

fn main() -> Result<(), serde_json::Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(&onejudge::sdk_schema::bundle())?
    );
    Ok(())
}
