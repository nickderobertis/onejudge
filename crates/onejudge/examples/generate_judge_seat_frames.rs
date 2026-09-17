//! Generate or check the committed judge-seat frame schema bundle.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&onejudge::sdk_schema::judge_seat_frame_bundle())?
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/judge-seat-frames.json");
    if std::env::args().nth(1).as_deref() == Some("--check") {
        let committed = std::fs::read_to_string(&path)?;
        if committed != generated {
            return Err(format!(
                "{} differs from generated frames; regenerate with `cargo run -q -p onejudge --features sdk-schema --example generate_judge_seat_frames > schemas/judge-seat-frames.json`",
                path.display()
            )
            .into());
        }
    } else {
        print!("{generated}");
    }
    Ok(())
}
