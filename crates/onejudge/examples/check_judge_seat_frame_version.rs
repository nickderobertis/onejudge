//! Refuse changed frame schemas whose bundle version did not increase.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let base = args
        .next()
        .ok_or("usage: check_judge_seat_frame_version BASE CURRENT")?;
    let current = args
        .next()
        .ok_or("usage: check_judge_seat_frame_version BASE CURRENT")?;
    let base = onemessagebus::SchemaBundle::from_json(&std::fs::read_to_string(base)?)?;
    let current = onemessagebus::SchemaBundle::from_json(&std::fs::read_to_string(current)?)?;
    if !onejudge::sdk_schema::frame_bundle_version_allows(&base, &current) {
        return Err(
            "judge-seat frame schemas changed without increasing the bundle version".into(),
        );
    }
    Ok(())
}
