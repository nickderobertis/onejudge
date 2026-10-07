# SimpleCov's configuration for bashcov, the shell coverage tool. Every bashcov
# run (`just _sh-test`) and the merge (`just _sh-coverage`) start from the
# repository root and load this file; only a project's run sets the variables.
# AGENTS.md, "Shell".
if (project = ENV["SHELL_COVERAGE_PROJECT"])
  # The scripts the project owns, minus its tests (`_sh-test` lists them): the
  # denominator, so a script no test runs counts as uncovered rather than absent.
  scripts = ENV.fetch("SHELL_COVERAGE_FILES").split("\n").reject(&:empty?)
  SimpleCov.coverage_dir File.join("target", "shell-coverage", project)
  # Absolute, as SimpleCov's root filter drops a relative path. `track_files` is
  # the one setting bashcov 4.0.0 reads (`SimpleCov.tracked_files`), so its
  # deprecation notice — which echoes the whole glob — is silenced at this call.
  verbose, $VERBOSE = $VERBOSE, nil
  begin
    SimpleCov.track_files "{#{scripts.map { |script| File.join(SimpleCov.root, script) }.join(",")}}"
  ensure
    $VERBOSE = verbose
  end
  # Everything else the suite traced: bats itself, its preprocessed tests, the
  # test helpers, and any other project's scripts.
  SimpleCov.skip { |source| !scripts.include?(source.project_filename.delete_prefix("/")) }
end
