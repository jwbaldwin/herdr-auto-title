use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wait_timeout::ChildExt;

use crate::policy::{Action, TabState, decide};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn run_from_env() -> Result<()> {
    let runtime = Runtime {
        herdr_bin: required_env("HERDR_BIN_PATH")?.into(),
        state_dir: required_env("HERDR_PLUGIN_STATE_DIR")?.into(),
        socket_path: required_env("HERDR_SOCKET_PATH")?,
        event_timeout: EVENT_TIMEOUT,
    };
    runtime.run_event(&required_env("HERDR_PLUGIN_EVENT_JSON")?)
}

struct Runtime {
    herdr_bin: PathBuf,
    state_dir: PathBuf,
    socket_path: String,
    event_timeout: Duration,
}

impl Runtime {
    fn run_event(&self, event_json: &str) -> Result<()> {
        let event: EventEnvelope = serde_json::from_str(event_json)?;
        let deadline = Instant::now() + self.event_timeout;
        match event.data {
            EventData::PaneAgentStatusChanged { pane_id } => self.update_title(&pane_id, deadline),
            EventData::TabCreated { tab } => self.remove_created_tab(&tab, deadline),
            EventData::TabClosed { tab_id } => {
                Store::open(&self.state_dir, &self.socket_path, deadline)?.remove_tab(&tab_id)
            }
            EventData::WorkspaceClosed { workspace_id } => {
                Store::open(&self.state_dir, &self.socket_path, deadline)?
                    .remove_workspace(&workspace_id)
            }
            EventData::Other => Ok(()),
        }
    }

    fn update_title(&self, pane_id: &str, deadline: Instant) -> Result<()> {
        let mut store = Store::open(&self.state_dir, &self.socket_path, deadline)?;
        let PaneResult { pane } = self.herdr_json(["pane", "get", pane_id], deadline)?;
        if pane.agent.as_deref() != Some("opencode") {
            return Ok(());
        }
        let Some(title) = pane.terminal_title_stripped.as_deref() else {
            return Ok(());
        };
        let TabResult { tab } = self.herdr_json(["tab", "get", &pane.tab_id], deadline)?;
        if tab.pane_count != 1 {
            return Ok(());
        }
        let state = store.get(&pane.tab_id);

        match decide(state, &tab.label, title) {
            Action::None => Ok(()),
            Action::Save(next) => store.save_tab(&pane.tab_id, next),
            Action::Rename { to, state } => {
                store.save_tab(&pane.tab_id, state)?;
                self.herdr(["tab", "rename", &pane.tab_id, &to], deadline)?;
                Ok(())
            }
        }
    }

    fn remove_created_tab(&self, created: &EventTab, deadline: Instant) -> Result<()> {
        let mut store = Store::open(&self.state_dir, &self.socket_path, deadline)?;
        if !store.contains(&created.tab_id) {
            return Ok(());
        }
        let current: TabResult = self.herdr_json(["tab", "get", &created.tab_id], deadline)?;
        if current.tab.label == created.label {
            store.remove_tab(&created.tab_id)?;
        }
        Ok(())
    }

    fn herdr_json<const N: usize, T: DeserializeOwned>(
        &self,
        args: [&str; N],
        deadline: Instant,
    ) -> Result<T> {
        let response: Response<T> = serde_json::from_slice(&self.herdr(args, deadline)?)?;
        Ok(response.result)
    }

    fn herdr<const N: usize>(&self, args: [&str; N], deadline: Instant) -> Result<Vec<u8>> {
        let mut child = Command::new(&self.herdr_bin)
            .args(args)
            .env("HERDR_SOCKET_PATH", &self.socket_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || child.wait_timeout(remaining)?.is_none() {
            child.kill()?;
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Herdr command timed out",
            )
            .into());
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim()).into())
        }
    }
}

#[derive(Deserialize)]
struct EventEnvelope {
    data: EventData,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventData {
    PaneAgentStatusChanged {
        pane_id: String,
    },
    TabCreated {
        tab: EventTab,
    },
    TabClosed {
        tab_id: String,
    },
    WorkspaceClosed {
        workspace_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct EventTab {
    tab_id: String,
    label: String,
}

#[derive(Deserialize)]
struct Response<T> {
    result: T,
}

#[derive(Deserialize)]
struct PaneResult {
    pane: Pane,
}

#[derive(Deserialize)]
struct Pane {
    agent: Option<String>,
    tab_id: String,
    terminal_title_stripped: Option<String>,
}

#[derive(Deserialize)]
struct TabResult {
    tab: Tab,
}

#[derive(Deserialize)]
struct Tab {
    label: String,
    pane_count: u32,
}

#[derive(Default, Deserialize, Serialize)]
struct StoredState {
    sessions: BTreeMap<String, BTreeMap<String, TabState>>,
}

struct Store {
    _lock: File,
    path: PathBuf,
    session: String,
    state: StoredState,
}

impl Store {
    fn open(state_dir: &Path, session: &str, deadline: Instant) -> Result<Self> {
        fs::create_dir_all(state_dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .truncate(false)
            .write(true)
            .open(state_dir.join("state.lock"))?;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "state lock timed out",
                        )
                        .into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error.into()),
            }
        }

        let path = state_dir.join("state.json");
        let state = match File::open(&path) {
            Ok(file) => serde_json::from_reader(file)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoredState::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            _lock: lock,
            path,
            session: session.to_owned(),
            state,
        })
    }

    fn get(&self, tab_id: &str) -> Option<&TabState> {
        self.state.sessions.get(&self.session)?.get(tab_id)
    }

    fn contains(&self, tab_id: &str) -> bool {
        self.get(tab_id).is_some()
    }

    fn save_tab(&mut self, tab_id: &str, state: TabState) -> Result<()> {
        self.state
            .sessions
            .entry(self.session.clone())
            .or_default()
            .insert(tab_id.to_owned(), state);
        self.flush()
    }

    fn remove_tab(&mut self, tab_id: &str) -> Result<()> {
        if let Some(tabs) = self.state.sessions.get_mut(&self.session) {
            if tabs.remove(tab_id).is_none() {
                return Ok(());
            }
            if tabs.is_empty() {
                self.state.sessions.remove(&self.session);
            }
            return self.flush();
        }
        Ok(())
    }

    fn remove_workspace(&mut self, workspace_id: &str) -> Result<()> {
        if let Some(tabs) = self.state.sessions.get_mut(&self.session) {
            let prefix = format!("{workspace_id}:");
            let previous_len = tabs.len();
            tabs.retain(|tab_id, _| !tab_id.starts_with(&prefix));
            if tabs.len() == previous_len {
                return Ok(());
            }
            if tabs.is_empty() {
                self.state.sessions.remove(&self.session);
            }
            return self.flush();
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        let temporary = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        let mut file = File::create(&temporary)?;
        serde_json::to_writer(&mut file, &self.state)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, &self.path)?;
        File::open(self.path.parent().expect("state path has a parent"))?.sync_all()?;
        Ok(())
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).map_err(|_| std::io::Error::other(format!("{name} is required")).into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    const STATUS_EVENT: &str = r#"{
        "event":"PaneAgentStatusChanged",
        "data":{
            "type":"pane_agent_status_changed",
            "pane_id":"w1:p1",
            "workspace_id":"w1",
            "agent_status":"idle",
            "agent":"opencode"
        }
    }"#;
    const TAB_CREATED_EVENT: &str = r#"{"event":"tab_created","data":{"type":"tab_created","tab":{"tab_id":"w1:t1","label":"1"}}}"#;

    #[test]
    fn lifecycle_event_renames_a_default_tab() {
        let fixture = Fixture::new("opencode", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "Fix OAuth callback"
        );
        assert!(fixture.state().contains("tracking"));
    }

    #[test]
    fn custom_label_is_never_renamed() {
        let fixture = Fixture::new("auth", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();

        assert!(!fixture.renamed.exists());
        assert!(fixture.state().contains("pinned"));
    }

    #[test]
    fn failed_rename_leaves_recoverable_tracking_state() {
        let fixture = Fixture::new("1", "Fix OAuth callback", true);
        assert!(fixture.runtime.run_event(STATUS_EVENT).is_err());

        let state = fixture.state();
        assert!(state.contains("tracking"));
        assert!(state.contains("Fix OAuth callback"));
    }

    #[test]
    fn tab_close_removes_saved_state() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture
            .runtime
            .run_event(
                r#"{"event":"TabClosed","data":{"type":"tab_closed","tab_id":"w1:t1","workspace_id":"w1"}}"#,
            )
            .unwrap();

        assert!(!fixture.state().contains("w1:t1"));
    }

    #[test]
    fn workspace_close_removes_all_tab_state() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture
            .runtime
            .run_event(
                r#"{"event":"workspace_closed","data":{"type":"workspace_closed","workspace_id":"w1"}}"#,
            )
            .unwrap();

        assert!(!fixture.state().contains("w1:t1"));
    }

    #[test]
    fn tab_creation_clears_reused_session_ids() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fs::write(&fixture.tab_label, "1").unwrap();
        fixture.runtime.run_event(TAB_CREATED_EVENT).unwrap();

        assert!(!fixture.state().contains("w1:t1"));
    }

    #[test]
    fn delayed_tab_creation_does_not_erase_newer_state() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.runtime.run_event(TAB_CREATED_EVENT).unwrap();

        assert!(fixture.state().contains("w1:t1"));
    }

    #[test]
    fn multi_pane_tab_is_left_unchanged() {
        let fixture = Fixture::new_with_panes("1", "Fix OAuth callback", false, 2);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        assert!(!fixture.renamed.exists());
    }

    #[test]
    fn stalled_herdr_command_times_out() {
        let mut fixture = Fixture::new("1", "Fix OAuth callback", false);
        fs::write(&fixture.runtime.herdr_bin, "#!/bin/sh\nsleep 5\n").unwrap();
        fixture.runtime.event_timeout = Duration::from_millis(20);

        assert!(fixture.runtime.run_event(STATUS_EVENT).is_err());
    }

    struct Fixture {
        _directory: TempDir,
        runtime: Runtime,
        renamed: PathBuf,
        tab_label: PathBuf,
    }

    impl Fixture {
        fn new(tab_label: &str, title: &str, fail_rename: bool) -> Self {
            Self::new_with_panes(tab_label, title, fail_rename, 1)
        }

        fn new_with_panes(
            tab_label: &str,
            title: &str,
            fail_rename: bool,
            pane_count: u32,
        ) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let renamed = directory.path().join("renamed");
            let tab_label_path = directory.path().join("tab-label");
            fs::write(&tab_label_path, tab_label).unwrap();
            let script = directory.path().join("herdr");
            let rename = if fail_rename {
                "exit 1".to_owned()
            } else {
                format!(
                    "printf '%s' \"$4\" > '{}'; printf '%s' \"$4\" > '{}'",
                    renamed.display(),
                    tab_label_path.display()
                )
            };
            fs::write(
                &script,
                format!(
                    r#"#!/bin/sh
case "$1:$2" in
  pane:get) printf '%s\n' '{{"result":{{"pane":{{"agent":"opencode","tab_id":"w1:t1","terminal_title_stripped":"{title}"}}}}}}' ;;
  tab:get) printf '{{"result":{{"tab":{{"label":"%s","pane_count":%s}}}}}}\n' "$(cat '{}')" '{pane_count}' ;;
  tab:rename) {rename} ;;
esac
"#,
                    tab_label_path.display()
                ),
            )
            .unwrap();
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
            let state_dir = directory.path().join("state");
            Self {
                runtime: Runtime {
                    herdr_bin: script,
                    state_dir,
                    socket_path: "test.sock".into(),
                    event_timeout: EVENT_TIMEOUT,
                },
                renamed,
                tab_label: tab_label_path,
                _directory: directory,
            }
        }

        fn state(&self) -> String {
            fs::read_to_string(self.runtime.state_dir.join("state.json")).unwrap()
        }
    }
}
