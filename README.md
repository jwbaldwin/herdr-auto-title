# herdr-auto-title

Keep a [Herdr](https://herdr.dev) tab synchronized with OpenCode's generated terminal title until you manually rename the tab.

## Behavior

- Numeric tab labels and `opencode` are treated as automatic defaults.
- An OpenCode lifecycle change reads the pane's latest `terminal_title_stripped` value and updates a single-pane tab.
- Any label outside the retained automatic defaults and generated titles is treated as manual and remains pinned.
- State is isolated by Herdr session socket and survives server restarts.
- Tabs with multiple panes are left unchanged because there is no unambiguous title owner.
- The plugin retains at most 16 recently used automatic labels per tab for crash recovery.

Herdr 0.7.5 does not provide a conditional tab-rename operation. A manual rename issued in the brief gap between the plugin's label check and automatic rename can be overwritten. Manually choosing a recent title tracked by the plugin, including the original numeric or `opencode` default, is also indistinguishable from Herdr restoring that automatic label.

## Install

Requirements: Herdr 0.7.5 or newer and Rust 1.85 or newer. Git is also required when installing directly from GitHub.

```sh
cargo build --release --locked
herdr plugin link .
```

The plugin is global to the current user and works in default and named Herdr sessions. Confirm registration and inspect event runs with:

```sh
herdr plugin list --plugin herdr-auto-title
herdr plugin log list --plugin herdr-auto-title
```

For a GitHub installation after this repository is published:

```sh
herdr plugin install jwbaldwin/herdr-auto-title
```

## Remove

For a local link:

```sh
herdr plugin unlink herdr-auto-title
```

For a GitHub-managed installation:

```sh
herdr plugin uninstall herdr-auto-title
```

Both commands leave plugin-owned state in Herdr's platform state directory.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
```
