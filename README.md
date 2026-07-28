# herdr-auto-title

Show agent status in managed [Herdr](https://herdr.dev) tabs and synchronize their labels with OpenCode's generated terminal title until you manually rename them.

## Behavior

- Numeric tab labels and `opencode` are treated as automatic defaults.
- An OpenCode lifecycle change reads the pane's latest `terminal_title_stripped` value and updates a single-pane tab.
- A tab becomes managed after the plugin observes OpenCode in one of its panes.
- Every managed label is prefixed with Herdr's authoritative tab status: `⣿` working, `◉` blocked, `●` done, `✓` idle, or `○` unknown.
- Any label outside the retained automatic defaults and generated titles is treated as manual. Its base text remains pinned while the status icon continues to update.
- State is isolated by Herdr session socket and survives server restarts.
- Tabs with multiple panes keep their base label and use Herdr's aggregate agent status because there is no unambiguous title owner.
- The plugin retains at most 16 recently used automatic labels per tab for crash recovery.
- Startup, focus, rename, status, and pane-topology events reconcile icons without polling.

Herdr 0.7.5 does not provide a conditional tab-rename operation. A manual rename issued in the brief gap between the plugin's label check and automatic rename can be overwritten. Manually choosing a recent title tracked by the plugin, including the original numeric or `opencode` default, is also indistinguishable from Herdr restoring that automatic label. Static icons are used because animating the working indicator would require polling or repeated tab renames.

Plugin hooks are best-effort. A dropped final hook can leave an icon stale until the next watched event or server startup. Herdr can also change `done` to `idle` when the outer terminal regains focus without emitting a plugin hook; the next status, tab-focus, rename, or topology event repairs the icon.

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
