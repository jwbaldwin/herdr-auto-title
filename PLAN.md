# Herdr Auto Title Plan

## Goal

Keep Herdr tab labels synchronized with OpenCode's generated terminal title until the user manually renames a tab. A manual name remains pinned across later OpenCode activity and Herdr restarts.

## Constraints

- Use only Herdr 0.7.5's supported plugin hooks.
- Never poll and never run on keyboard input.
- Ignore agents other than OpenCode.
- Treat numeric labels and `opencode` as automatic defaults. Treat any other initial label as intentional.
- Persist state under `HERDR_PLUGIN_STATE_DIR` and distinguish named Herdr sessions that reuse tab IDs.
- Serialize concurrent event processes so an automatic rename cannot be mistaken for a manual rename.
- Treat event payloads as wake-ups, then reread the authoritative pane title and tab label while holding the state lock.
- Persist the 16 most recently used automatic labels before asking Herdr to rename so a recently restored label remains recognized as automatic.
- Accept that Herdr 0.7.5 has no conditional tab rename: a manual rename issued in the brief gap between the authoritative read and automatic rename can be overwritten.
- A manual rename to one of the 16 retained automatic labels is indistinguishable from a restored label and remains automatic.

## Phase 1: Title Policy (complete)

Build a pure, tested state machine for:

- accepting generated titles on default tabs,
- pinning unexpected manual names,
- committing automatic state before the external rename for crash recovery,
- ignoring empty titles and already-matching labels.

Acceptance: unit tests cover every transition without filesystem, process, or Herdr dependencies.

## Phase 2: Herdr Runtime (complete)

Add the smallest runtime adapter that:

- parses `HERDR_PLUGIN_EVENT_JSON`,
- reacts to OpenCode lifecycle, tab creation/closure, and workspace closure events,
- reads pane and tab state through `HERDR_BIN_PATH`,
- stores state atomically under an exclusive file lock,
- asks Herdr to rename only when the policy returns a rename decision.

Acceptance: adapter tests use a fake Herdr executable and isolated state directory; no live session required.

## Phase 3: Plugin Package (complete)

Add the Herdr manifest, documentation, release profile, and live verification:

- build the release binary,
- link the local plugin,
- verify manifest and event-hook registration,
- exercise automatic and manual rename behavior in a disposable named Herdr session,
- document installation, behavior, limits, and removal.

Acceptance: formatting, linting, tests, release build, plugin link, and disposable-session checks all pass.
