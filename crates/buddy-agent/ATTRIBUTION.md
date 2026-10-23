# Attribution and porting notes

`buddy-agent` is the Drops/Buddy agent engine. Its design is a Rust port of the
agent loop and coding-tool ideas from **Pi** (`@earendil-works/pi`), which is
MIT-licensed. Drops itself is MIT-licensed.

## Upstream reference

- Repository: <https://github.com/earendil-works/pi>
- Commit used for this port: `3953e9ef497024d13c6f4aa646d0adda2e37b0f8`
  (2026-08-13), recorded in `references/pi-revision.json`.
- `references/pi-agent-loop.ts` is the downloaded upstream loop that was ported.
- Related upstream files inspected for tool and session behaviour:
  - `packages/agent/src/types.ts` (agent context, tool hooks, event model)
  - `packages/coding-agent/src/core/tools/read.ts`
  - `packages/coding-agent/src/core/tools/edit.ts`
  - `packages/coding-agent/src/core/tools/write.ts`
  - `packages/coding-agent/src/core/tools/bash.ts`
  - `packages/coding-agent/src/core/session-manager.ts` and
    `packages/coding-agent/docs/session-format.md`
  - `packages/coding-agent/docs/skills.md`
- Upstream license: MIT, Copyright (c) 2025 Mario Zechner. The full text is at
  `references/pi-LICENSE`; it covers Pi and, by the license terms, this ported
  work.

## Concepts ported

1. **Turn loop.** One assistant response per turn; execute the tool calls it
   contains; append tool-result messages; request the next assistant response.
   The loop ends when the assistant emits no tool calls, when the caller's turn
   budget is exhausted, or when cancellation is observed. Pi's separate
   "continue" entry point is folded into the single `run` contract in
   `docs/contracts.md`.
2. **Prepare → execute → finalize.** A call is resolved against the registry,
   its arguments are decoded/validated, policy (here: risk/approval) runs, then
   the tool executes, and a final result is appended. Unknown tools, bad JSON,
   missing names, denied approvals, and tool exceptions all become model-facing
   error results rather than aborting the run.
3. **Truncation policy for an LLM.** File output is bounded by lines and bytes,
   includes a continuation hint (`Use offset=N to continue`), and bash output is
   capped with an explicit truncation marker. These follow Pi's read/bash
   truncation behaviour.
4. **Streaming events.** Text and reasoning fragments are forwarded as they
   arrive; tool activity is reported with the call id, name, and exact argument
   value so the UI can render it. Final tool calls are taken from the completed
   assistant message, not from partial stream fragments.
5. **JSONL sessions.** Append-only one-object-per-line logs with a session
   header and typed message entries. This mirrors Pi's session-file shape in a
   deliberately simpler, linear form (`type: "session"` header plus
   `type: "message"` entries).
6. **Layered markdown and skills.** Global/project instruction files are
   concatenated from least to most specific, and `SKILL.md` files are indexed by
   name/description with full bodies loaded on demand. The frontmatter parser
   accepts Pi-style `name`/`description` (including folded `>` blocks).
7. **Exact approvals.** The approval event carries the tool name and the decoded
   arguments, mirroring Pi's pre-execution hooks. One decision is consumed per
   call; absence or denial is a deny, never a bypass.
8. **Cancellation and process cleanup.** Every command runs in its own process
   group and is killed with `SIGKILL` to the group on cancellation/timeout, so
   shell descendants cannot outlive the agent. Pi likewise kills the process
   tree on abort.

## Deliberate differences

- **Sequential tools only.** Pi can run a tool batch in parallel. Drops runs
  calls in order because approvals are interactive and each decision is
  ordered; this also keeps shell/file mutations deterministic.
- **Linear sessions.** Pi supports tree branching, compaction, fork/clone, and
  mid-session model/tool-loadout entries. Drops uses an append-only linear
  transcript and a simple size-bounded trim, which is enough for the daemon's
  per-blob sessions.
- **No dynamic tool-loadout system messages.** The tool set is fixed for a run
  (`allow_shell` plus configured external tools and discovered skills), so there
  is nothing to announce mid-session.
- **No steering/follow-up queues, hooks, extensions, codemode, or image
  pipelines.** Those are outside the engine contract; images are treated as
  unavailable text context by the provider layer.
- **Tool schemas use `old_string`/`new_string`.** Pi's primary edit shape is
  `edits: [{oldText,newText}]`. Drops advertises the simpler two-string shape
  for small local models but still accepts Pi's shape and the
  `oldText`/`newText` aliases.
- **External tools are a new, Drops-specific stdio protocol** (Pi uses its
  extension API). It is intentionally shell-script friendly.

## External tool protocol

The engine spawns `command` as an argv vector (no shell) with the working
directory set to the confined project root. For each call it writes exactly one
JSON line to stdin:

```json
{"name":"daily_note","arguments":{"text":"hello"}}
```

The tool writes one JSON object to stdout:

```json
{"result":"appended to daily-log.md"}
```

or, on failure (normally with a non-zero exit status):

```json
{"error":"could not append: permission denied"}
```

Accepted response variants: `result` (any JSON value, strings are preferred),
`error` (string), `content` (string or text/image block array), `text`, or a
bare string/plain-text stdout. Log lines before the final JSON object are
tolerated. A non-zero exit status always produces an error result. Stderr is
diagnostic only and is never sent to the model. Each call is a fresh process, so
no call id is needed. The example in `examples/external-tool.py` implements this
protocol.

## Risk and approval policy

| Tool | Risk | Approval |
|---|---|---|
| `read`, `skill` | read-only | none |
| `write`, `edit` | mutating | one per call |
| `bash` | dangerous | one per call, and only present when `allow_shell` |
| external `read-only`/`safe` | read-only | none |
| external `mutating`/`write` | mutating | one per call |
| external `dangerous` or unknown | dangerous | one per call |

Approval events contain `name`, `arguments` (the exact decoded value), and
`risk`. The daemon answers with one `bool`. A closed approvals channel denies.

## Security model

- The project root is canonicalized once per run. Every model-supplied file path
  is lexically normalized, resolved through its nearest existing ancestor, and
  required to be component-wise inside the root. This rejects `..`, absolute
  paths outside the root, and symlink escapes (including through a symlinked
  parent for a not-yet-existing file).
- Bash is absent from the tool definitions when `allow_shell` is false, and the
  tool itself checks the flag again.
- Bash uses `bash -c` (not a login shell), removes `BASH_ENV`, runs in the
  project root, and is bounded by a timeout (default 120 s, maximum 600 s).
- External tools run with an argv vector, never a shell string, and are bounded
  by a 60 s timeout.
- Every command runs in a dedicated process group. Cancellation and timeout
  `SIGKILL` the group and reap the leader; a drop guard does the same if the
  engine future is dropped. A regression test forks a background shell that
  would write a file after a delay and asserts cancellation prevents the write.
- File contents and bash output are bounded before being sent to the model.
- Session ids accept only `[A-Za-z0-9._-]`, must start alphanumeric, and reject
  `..`, separators, and whitespace; `session_path` always returns a direct child
  of the session directory.
- The API key is never read by this crate; `buddy-ai` reads only the configured
  environment variable.

## Tests

`cargo test -p buddy-agent` covers path confinement (traversal, absolute,
symlinked file/directory, missing leaf), file read/write/edit semantics, JSONL
round-trips and history trimming, wire-message sanitization, malformed and
unknown tool calls, skill discovery/override/symlink escape, external-protocol
parsing and execution, process-group kill behaviour, and end-to-end runs with a
mocked provider (read-only execution, exact approval events, one approval per
call, denial, allow_shell gating, bash danger, max turns, provider errors, and
cancellation before and during approval).
