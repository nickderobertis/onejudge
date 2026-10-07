# SimpleCov's configuration for bashcov, the shell coverage tool. Every bashcov
# run (`just _sh-test`) and the merge (`just _sh-coverage`) start from the
# repository root and load this file; only a project's run sets the variables.
# AGENTS.md, "Shell".
if (project = ENV["SHELL_COVERAGE_PROJECT"])
  unless project.match?(/\A[a-z0-9][a-z0-9-]*\z/)
    abort "shell coverage: SHELL_COVERAGE_PROJECT=#{project.inspect} is not an Nx project name (a-z, 0-9, -)\n" \
          "ACTION: run the project's test target (just _sh-test), which sets it"
  end
  # The scripts the project owns, minus its tests (`_sh-test` lists them): the
  # denominator, so a script no test runs counts as uncovered rather than absent.
  scripts = ENV.fetch("SHELL_COVERAGE_FILES", "").split("\n").reject(&:empty?)
  # Each must be spelled as the skip filter below compares it (canonical,
  # relative), resolve inside the repository, and hold no glob syntax, as it
  # goes into the track_files glob below.
  root = File.realpath(SimpleCov.root)
  outside = scripts.reject do |script|
    script == Pathname.new(script).cleanpath.to_s && !script.start_with?("/", "../") &&
      !script.match?(/[*?\[\]{},\\]/) && File.file?(script) && File.realpath(script).start_with?("#{root}/")
  end
  unless !scripts.empty? && outside.empty?
    abort "shell coverage: SHELL_COVERAGE_FILES must list the project's scripts, each an existing file inside the " \
          "repository, spelled canonically relative to its root, with no glob characters; got " \
          "#{outside.empty? ? "none" : outside.map(&:inspect).join(", ")}\n" \
          "ACTION: run the project's test target (just _sh-test), which lists them with scripts/shell-files.sh"
  end
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
