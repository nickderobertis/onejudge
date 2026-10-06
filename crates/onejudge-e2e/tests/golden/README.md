# Golden documents

`notes/` holds one capture per note journey `tests/notes.rs` drives, each taken
from the tree before the note contract moved onto the `onemessagebus` channel, so
every journey is held to what it delivered then. Paths that vary per run are
normalized to `{{TMP}}` and `{{ECHO}}`. Never recapture them from the tree under
test.
