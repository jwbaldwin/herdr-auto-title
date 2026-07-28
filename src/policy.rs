use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum TabState {
    Tracking {
        labels: VecDeque<String>,
    },
    Pinned {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

const MAX_TRACKED_LABELS: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentStatus {
    Working,
    Blocked,
    Done,
    Idle,
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    fn icon(self) -> &'static str {
        match self {
            Self::Working => "⣿",
            Self::Blocked => "◉",
            Self::Done => "●",
            Self::Idle => "✓",
            Self::Unknown => "○",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Save(TabState),
    Rename { to: String, state: TabState },
}

pub(crate) fn decide(
    state: Option<&TabState>,
    current_label: &str,
    generated_title: Option<&str>,
    status: AgentStatus,
) -> Action {
    let generated_title = generated_title
        .map(str::trim)
        .filter(|title| !title.is_empty());

    let (base_label, next) = match state {
        Some(TabState::Pinned { label }) => {
            if let Some(label) = label
                && is_owned_label(current_label, label)
            {
                let base_label = label.to_owned();
                let next = TabState::Pinned {
                    label: Some(base_label.clone()),
                };
                (base_label, next)
            } else if is_default_label(current_label) {
                let mut labels = VecDeque::from([current_label.to_owned()]);
                let base_label = generated_title.unwrap_or(current_label).to_owned();
                remember(&mut labels, &base_label);
                (base_label, TabState::Tracking { labels })
            } else {
                let base_label = current_label.to_owned();
                let next = TabState::Pinned {
                    label: Some(base_label.clone()),
                };
                (base_label, next)
            }
        }
        Some(TabState::Tracking { labels }) => {
            let Some(observed_label) = tracked_base(current_label, labels) else {
                if is_default_label(current_label) {
                    let mut labels = VecDeque::from([current_label.to_owned()]);
                    let base_label = generated_title.unwrap_or(current_label).to_owned();
                    remember(&mut labels, &base_label);
                    let next = TabState::Tracking { labels };
                    return finish(state, current_label, base_label, next, status);
                }
                let base_label = current_label.to_owned();
                let next = TabState::Pinned {
                    label: Some(base_label.clone()),
                };
                return finish(state, current_label, base_label, next, status);
            };
            let mut labels = labels.clone();
            remember(&mut labels, &observed_label);
            let base_label = generated_title.unwrap_or(&observed_label).to_owned();
            remember(&mut labels, &base_label);
            while labels.len() > MAX_TRACKED_LABELS {
                labels.pop_front();
            }
            (base_label, TabState::Tracking { labels })
        }
        None if is_default_label(current_label) => {
            let mut labels = VecDeque::from([current_label.to_owned()]);
            let base_label = generated_title.unwrap_or(current_label).to_owned();
            remember(&mut labels, &base_label);
            (base_label, TabState::Tracking { labels })
        }
        None => {
            let base_label = current_label.to_owned();
            let next = TabState::Pinned {
                label: Some(base_label.clone()),
            };
            (base_label, next)
        }
    };

    finish(state, current_label, base_label, next, status)
}

fn finish(
    state: Option<&TabState>,
    current_label: &str,
    base_label: String,
    next: TabState,
    status: AgentStatus,
) -> Action {
    let desired = format!("{} {base_label}", status.icon());

    if current_label == desired {
        if state == Some(&next) {
            Action::None
        } else {
            Action::Save(next)
        }
    } else {
        Action::Rename {
            to: desired,
            state: next,
        }
    }
}

fn tracked_base(current_label: &str, labels: &VecDeque<String>) -> Option<String> {
    if labels.iter().any(|label| label == current_label) {
        return Some(current_label.to_owned());
    }
    let base = strip_status(current_label)?;
    labels
        .iter()
        .any(|label| label == base)
        .then(|| base.to_owned())
}

fn is_owned_label(current_label: &str, base_label: &str) -> bool {
    current_label == base_label || strip_status(current_label) == Some(base_label)
}

fn strip_status(label: &str) -> Option<&str> {
    ["⣿", "◉", "●", "✓", "○"]
        .into_iter()
        .find_map(|icon| label.strip_prefix(icon)?.strip_prefix(' '))
}

fn remember(labels: &mut VecDeque<String>, label: &str) {
    labels.retain(|known| known != label);
    labels.push_back(label.to_owned());
}

fn is_default_label(label: &str) -> bool {
    label.eq_ignore_ascii_case("opencode") || label.parse::<u64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_title_replaces_default_labels() {
        for label in ["1", "opencode", "OpenCode"] {
            assert_eq!(
                decide(
                    None,
                    label,
                    Some("Fix OAuth callback"),
                    AgentStatus::Working,
                ),
                rename(&[label, "Fix OAuth callback"], "⣿ Fix OAuth callback")
            );
        }
    }

    #[test]
    fn every_status_has_a_stable_icon() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        for (status, icon) in [
            (AgentStatus::Working, "⣿"),
            (AgentStatus::Blocked, "◉"),
            (AgentStatus::Done, "●"),
            (AgentStatus::Idle, "✓"),
            (AgentStatus::Unknown, "○"),
        ] {
            assert_eq!(
                decide(Some(&state), "Fix OAuth callback", None, status),
                rename(
                    &["1", "Fix OAuth callback"],
                    &format!("{icon} Fix OAuth callback")
                )
            );
        }
    }

    #[test]
    fn custom_initial_label_is_pinned_but_decorated() {
        assert_eq!(
            decide(
                None,
                "auth",
                Some("Fix OAuth callback"),
                AgentStatus::Blocked,
            ),
            Action::Rename {
                to: "◉ auth".to_owned(),
                state: pinned("auth"),
            }
        );
    }

    #[test]
    fn tracked_title_advances() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        assert_eq!(
            decide(
                Some(&state),
                "✓ Fix OAuth callback",
                Some("Add regression test"),
                AgentStatus::Working,
            ),
            rename(
                &["1", "Fix OAuth callback", "Add regression test"],
                "⣿ Add regression test"
            )
        );
    }

    #[test]
    fn status_changes_do_not_change_retained_titles() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        assert_eq!(
            decide(
                Some(&state),
                "⣿ Fix OAuth callback",
                None,
                AgentStatus::Idle,
            ),
            rename(&["1", "Fix OAuth callback"], "✓ Fix OAuth callback")
        );
    }

    #[test]
    fn every_retained_label_survives_decorated_restore() {
        let state = tracking(&["1", "First title", "Second title"]);
        for current in ["1", "First title", "Second title"] {
            assert!(matches!(
                decide(
                    Some(&state),
                    &format!("● {current}"),
                    Some("Latest title"),
                    AgentStatus::Working,
                ),
                Action::Rename { .. }
            ));
        }
    }

    #[test]
    fn tracked_history_is_bounded() {
        let mut state = tracking(&["1"]);
        let mut current = "✓ 1".to_owned();
        for index in 0..=MAX_TRACKED_LABELS {
            let title = format!("Title {index}");
            let Action::Rename { to, state: next } =
                decide(Some(&state), &current, Some(&title), AgentStatus::Working)
            else {
                panic!("expected rename");
            };
            state = next;
            current = to;
        }

        let TabState::Tracking { labels } = state else {
            panic!("expected tracking state");
        };
        assert_eq!(labels.len(), MAX_TRACKED_LABELS);
        assert_eq!(labels.back().map(String::as_str), Some("Title 16"));
    }

    #[test]
    fn retention_keeps_the_observed_and_generated_labels() {
        let labels = (0..MAX_TRACKED_LABELS)
            .map(|index| format!("Title {index}"))
            .collect();
        let state = TabState::Tracking { labels };
        let Action::Rename { state, .. } = decide(
            Some(&state),
            "✓ Title 0",
            Some("Latest"),
            AgentStatus::Working,
        ) else {
            panic!("expected rename");
        };
        let TabState::Tracking { labels } = state else {
            panic!("expected tracking state");
        };

        assert_eq!(labels.len(), MAX_TRACKED_LABELS);
        assert!(labels.iter().any(|label| label == "Title 0"));
        assert_eq!(labels.back().map(String::as_str), Some("Latest"));
    }

    #[test]
    fn oversized_state_is_normalized() {
        let labels = (0..20).map(|index| format!("Title {index}")).collect();
        let state = TabState::Tracking { labels };
        let Action::Save(TabState::Tracking { labels }) = decide(
            Some(&state),
            "✓ Title 19",
            Some("Title 19"),
            AgentStatus::Idle,
        ) else {
            panic!("expected normalized state");
        };

        assert_eq!(labels.len(), MAX_TRACKED_LABELS);
        assert_eq!(labels.back().map(String::as_str), Some("Title 19"));
    }

    #[test]
    fn unknown_label_pins_a_tracked_tab() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        assert_eq!(
            decide(
                Some(&state),
                "auth",
                Some("Add regression test"),
                AgentStatus::Idle,
            ),
            Action::Rename {
                to: "✓ auth".to_owned(),
                state: pinned("auth"),
            }
        );
    }

    #[test]
    fn pinned_label_keeps_status_updates_and_manual_renames() {
        let state = pinned("auth");
        assert_eq!(
            decide(Some(&state), "◉ auth", None, AgentStatus::Idle),
            Action::Rename {
                to: "✓ auth".to_owned(),
                state: state.clone(),
            }
        );
        assert_eq!(
            decide(Some(&state), "billing", None, AgentStatus::Done),
            Action::Rename {
                to: "● billing".to_owned(),
                state: pinned("billing"),
            }
        );
    }

    #[test]
    fn title_beginning_with_an_icon_is_preserved() {
        let state = tracking(&["1", "● incident"]);
        assert_eq!(
            decide(Some(&state), "✓ ● incident", None, AgentStatus::Working,),
            rename(&["1", "● incident"], "⣿ ● incident")
        );
    }

    #[test]
    fn empty_title_still_updates_status() {
        let state = tracking(&["1"]);
        assert_eq!(
            decide(Some(&state), "✓ 1", Some("  "), AgentStatus::Blocked),
            rename(&["1"], "◉ 1")
        );
    }

    #[test]
    fn old_pinned_state_is_migrated_from_the_observed_label() {
        let state: TabState = serde_json::from_str(r#"{"mode":"pinned"}"#).unwrap();
        assert_eq!(
            decide(Some(&state), "auth", None, AgentStatus::Unknown),
            Action::Rename {
                to: "○ auth".to_owned(),
                state: pinned("auth"),
            }
        );
    }

    #[test]
    fn pinned_state_resets_when_a_new_tab_reuses_a_default_label() {
        let state = pinned("auth");
        assert_eq!(
            decide(
                Some(&state),
                "1",
                Some("Fix OAuth callback"),
                AgentStatus::Working,
            ),
            rename(&["1", "Fix OAuth callback"], "⣿ Fix OAuth callback")
        );
    }

    #[test]
    fn stale_tracking_state_resets_when_a_new_tab_reuses_a_default_label() {
        let state = tracking(&["Old title"]);
        assert_eq!(
            decide(
                Some(&state),
                "1",
                Some("Fix OAuth callback"),
                AgentStatus::Working,
            ),
            rename(&["1", "Fix OAuth callback"], "⣿ Fix OAuth callback")
        );
    }

    #[test]
    fn future_status_values_fall_back_to_unknown() {
        assert_eq!(
            serde_json::from_str::<AgentStatus>(r#""paused""#).unwrap(),
            AgentStatus::Unknown
        );
    }

    #[test]
    fn matching_decorated_label_is_unchanged() {
        let state = tracking(&["OpenCode"]);
        assert_eq!(
            decide(Some(&state), "✓ OpenCode", None, AgentStatus::Idle),
            Action::None
        );
    }

    fn tracking(labels: &[&str]) -> TabState {
        TabState::Tracking {
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        }
    }

    fn pinned(label: &str) -> TabState {
        TabState::Pinned {
            label: Some(label.to_owned()),
        }
    }

    fn rename(labels: &[&str], to: &str) -> Action {
        Action::Rename {
            to: to.to_owned(),
            state: tracking(labels),
        }
    }
}
