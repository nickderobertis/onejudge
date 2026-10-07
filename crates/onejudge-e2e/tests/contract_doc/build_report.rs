fn build_report(
    engine: &onejudge::Engine<'_>,
    conversation: &onejudge::Conversation,
) -> onejudge::Result<onejudge::Report> {
    let outcome = engine.run(conversation)?;
    let verdict = engine.judge_boolean("the change was committed", &outcome.transcript)?;
    let report = outcome.into_report(vec![onejudge::NamedVerdict::new(
        "the change was committed",
        onejudge::JudgeKind::Boolean,
        verdict,
    )]);
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
    Ok(report)
}
