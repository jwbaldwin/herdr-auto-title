use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum TabState {
    Tracking { labels: VecDeque<String> },
    Pinned,
}

const MAX_TRACKED_LABELS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Save(TabState),
    Rename { to: String, state: TabState },
}

pub(crate) fn decide(
    state: Option<&TabState>,
    current_label: &str,
    generated_title: &str,
) -> Action {
    let generated_title = generated_title.trim();
    if generated_title.is_empty() {
        return Action::None;
    }

    let mut labels = match state {
        Some(TabState::Pinned) => return Action::None,
        Some(TabState::Tracking { labels })
            if labels.iter().any(|label| label == current_label) =>
        {
            labels.clone()
        }
        Some(TabState::Tracking { .. }) => return Action::Save(TabState::Pinned),
        None if is_default_label(current_label) => VecDeque::from([current_label.to_owned()]),
        None => return Action::Save(TabState::Pinned),
    };
    remember(&mut labels, current_label);
    remember(&mut labels, generated_title);
    while labels.len() > MAX_TRACKED_LABELS {
        labels.pop_front();
    }
    let next = TabState::Tracking { labels };

    if current_label == generated_title {
        if state == Some(&next) {
            Action::None
        } else {
            Action::Save(next)
        }
    } else {
        Action::Rename {
            to: generated_title.to_owned(),
            state: next,
        }
    }
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
                decide(None, label, "Fix OAuth callback"),
                rename(&[label, "Fix OAuth callback"], "Fix OAuth callback")
            );
        }
    }

    #[test]
    fn custom_initial_label_is_pinned() {
        assert_eq!(
            decide(None, "auth", "Fix OAuth callback"),
            Action::Save(TabState::Pinned)
        );
    }

    #[test]
    fn tracked_title_advances() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        assert_eq!(
            decide(Some(&state), "Fix OAuth callback", "Add regression test"),
            rename(
                &["1", "Fix OAuth callback", "Add regression test"],
                "Add regression test"
            )
        );
    }

    #[test]
    fn every_retained_label_survives_restore() {
        let state = tracking(&["1", "First title", "Second title"]);
        for current in ["1", "First title", "Second title"] {
            assert!(matches!(
                decide(Some(&state), current, "Latest title"),
                Action::Rename { .. }
            ));
        }
    }

    #[test]
    fn tracked_history_is_bounded() {
        let mut state = tracking(&["1"]);
        let mut current = "1".to_owned();
        for index in 0..=MAX_TRACKED_LABELS {
            let title = format!("Title {index}");
            let Action::Rename { state: next, .. } = decide(Some(&state), &current, &title) else {
                panic!("expected rename");
            };
            state = next;
            current = title;
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
        let Action::Rename { state, .. } = decide(Some(&state), "Title 0", "Latest") else {
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
        let Action::Save(TabState::Tracking { labels }) =
            decide(Some(&state), "Title 19", "Title 19")
        else {
            panic!("expected normalized state");
        };

        assert_eq!(labels.len(), MAX_TRACKED_LABELS);
        assert_eq!(labels.back().map(String::as_str), Some("Title 19"));
    }

    #[test]
    fn unknown_label_pins_a_tracked_tab() {
        let state = tracking(&["1", "Fix OAuth callback"]);
        assert_eq!(
            decide(Some(&state), "auth", "Add regression test"),
            Action::Save(TabState::Pinned)
        );
    }

    #[test]
    fn matching_title_tracks_without_renaming_or_duplicates() {
        let state = tracking(&["OpenCode"]);
        assert_eq!(decide(Some(&state), "OpenCode", "OpenCode"), Action::None);
    }

    #[test]
    fn empty_title_and_pinned_tabs_are_unchanged() {
        assert_eq!(decide(None, "1", "  "), Action::None);
        assert_eq!(decide(Some(&tracking(&["1"])), "1", "  "), Action::None);
        assert_eq!(
            decide(Some(&TabState::Pinned), "auth", "New title"),
            Action::None
        );
    }

    fn tracking(labels: &[&str]) -> TabState {
        TabState::Tracking {
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        }
    }

    fn rename(labels: &[&str], to: &str) -> Action {
        Action::Rename {
            to: to.to_owned(),
            state: tracking(labels),
        }
    }
}
