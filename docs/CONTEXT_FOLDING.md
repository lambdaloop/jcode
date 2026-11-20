# Model-driven context folding

Context folding is opt-in. Add this to `~/.jcode/config.toml` (or
`$JCODE_HOME/config.toml` when using a custom home):

```toml
[compaction]
mode = "model_driven"
```

The default remains `reactive`. `proactive` and `semantic` are also available.
`model_driven` is the canonical serialized name, and `fold` is an alias.
For a session-only change, use `/compact mode model_driven` or
`/compact mode fold`. `/compact mode` shows the current mode.

## Native tools

In model-driven mode, the model manages which conversation spans remain in
its active context through native tools. These are tool calls, not shell
commands or slash commands.

### `compress`

Replace a contiguous span of conversation messages in active context with a
summary. Supply `start`, `end`, and `summary`:

```json
{"start":"m00001","end":"m00005","summary":"Investigated the parser. Preserve the failing input and fix the boundary check next."}
```

`start` and `end` are message references, such as `m00001`, not numeric array
indices. Both endpoints are inclusive. Use references from the current
context rather than guessing them. The summary should preserve decisions,
constraints, outstanding tasks, and details needed to continue. Keep the
returned fold ID if the original span may be needed later.

A new fold can cover several existing folds as long as it includes each one
in full. The replacement ID restores the entire span, including the original
messages behind the older folds.

### `decompress`

Restore a folded span using its numeric `fold_id`:

```json
{"fold_id":1}
```

Pass a number, not a quoted string or a message reference. Restoring a span
increases active context size, so inspect context usage before restoring
large spans.

### `context_status`

Inspect the current context and folds without changing them:

```json
{}
```

Use this tool to check available fold IDs and context state before deciding
what to compress or restore.

## Limits and tradeoffs

Folding reduces what is sent in active model context, not the model's finite
context-window limit. A summary can omit important details, so restore the
original span when exact wording or evidence matters. Retained originals
still consume storage, and retention remains subject to session persistence
and available disk space. Folding does not provide unlimited physical
storage or guarantee that every future request will fit the model window.
If emergency compaction moves past a fold, use `conversation_search` to
recover excerpts from the transcript rather than reopening the whole span.

Folds are saved with the session. If recovery inserts a missing tool result,
folds outside the insertion are preserved. An insertion inside a folded range
invalidates that fold so the model can see the repaired transcript.
