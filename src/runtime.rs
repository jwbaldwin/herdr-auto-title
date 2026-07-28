use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt;
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

use crate::policy::{Action, AgentStatus, TabState, decide};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn run_from_env() -> Result<()> {
    let runtime = Runtime {
        herdr_bin: required_env("HERDR_BIN_PATH")?.into(),
        state_dir: required_env("HERDR_PLUGIN_STATE_DIR")?.into(),
        socket_path: required_env("HERDR_SOCKET_PATH")?,
        event_timeout: EVENT_TIMEOUT,
    };
    if env::var("HERDR_PLUGIN_EVENT").as_deref() == Ok("startup") {
        runtime.reconcile_startup()
    } else {
        runtime.run_event(&required_env("HERDR_PLUGIN_EVENT_JSON")?)
    }
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
            EventData::PaneAgentStatusChanged { pane_id } => {
                self.update_from_pane(&pane_id, deadline)
            }
            EventData::TabCreated { tab } => self.remove_created_tab(&tab, deadline),
            EventData::TabFocused { tab_id } | EventData::TabRenamed { tab_id } => {
                self.update_known_tab(&tab_id, deadline)
            }
            EventData::PaneMoved {
                previous_tab_id,
                pane,
            } => {
                let mut first_error = self.update_from_pane(&pane.pane_id, deadline).err();
                if previous_tab_id != pane.tab_id {
                    if let Err(error) = self.update_known_tab(&previous_tab_id, deadline) {
                        first_error.get_or_insert(error);
                    }
                }
                first_error.map_or(Ok(()), Err)
            }
            EventData::PaneClosed { workspace_id } => {
                self.update_workspace_tabs(&workspace_id, deadline)
            }
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

    fn reconcile_startup(&self) -> Result<()> {
        let deadline = Instant::now() + self.event_timeout;
        let tab_ids = Store::open(&self.state_dir, &self.socket_path, deadline)?.tab_ids();
        let mut first_error = None;
        for tab_id in tab_ids {
            let deadline = Instant::now() + self.event_timeout;
            if let Err(error) = self.update_known_tab(&tab_id, deadline) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn update_from_pane(&self, pane_id: &str, deadline: Instant) -> Result<()> {
        let mut store = Store::open(&self.state_dir, &self.socket_path, deadline)?;
        let PaneResult { pane } = self.herdr_json(["pane", "get", pane_id], deadline)?;
        let is_opencode = pane.agent.as_deref() == Some("opencode");
        if !is_opencode && !store.contains(&pane.tab_id) {
            return Ok(());
        }
        let TabResult { tab } = self.herdr_json(["tab", "get", &pane.tab_id], deadline)?;
        let title = if is_opencode && tab.pane_count == 1 {
            pane.terminal_title_stripped.as_deref()
        } else {
            None
        };
        self.apply_tab(&mut store, &pane.tab_id, &tab, title, deadline)
    }

    fn update_known_tab(&self, tab_id: &str, deadline: Instant) -> Result<()> {
        let mut store = Store::open(&self.state_dir, &self.socket_path, deadline)?;
        self.update_known_tab_with_store(&mut store, tab_id, deadline)
    }

    fn update_known_tab_with_store(
        &self,
        store: &mut Store,
        tab_id: &str,
        deadline: Instant,
    ) -> Result<()> {
        if !store.contains(tab_id) {
            return Ok(());
        }
        let tab = match self.herdr_json(["tab", "get", tab_id], deadline) {
            Ok(TabResult { tab }) => tab,
            Err(error) if is_tab_not_found(error.as_ref()) => return store.remove_tab(tab_id),
            Err(error) => return Err(error),
        };
        self.apply_tab(store, tab_id, &tab, None, deadline)
    }

    fn update_workspace_tabs(&self, workspace_id: &str, deadline: Instant) -> Result<()> {
        let mut store = Store::open(&self.state_dir, &self.socket_path, deadline)?;
        let tab_ids = store.tab_ids_in_workspace(workspace_id);
        let mut first_error = None;
        for tab_id in tab_ids {
            if let Err(error) = self.update_known_tab_with_store(&mut store, &tab_id, deadline) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn apply_tab(
        &self,
        store: &mut Store,
        tab_id: &str,
        tab: &Tab,
        generated_title: Option<&str>,
        deadline: Instant,
    ) -> Result<()> {
        let state = store.get(tab_id);

        match decide(state, &tab.label, generated_title, tab.agent_status) {
            Action::None => Ok(()),
            Action::Save(next) => store.save_tab(tab_id, next),
            Action::Rename { to, state } => {
                store.save_tab(tab_id, state)?;
                self.herdr(["tab", "rename", tab_id, &to], deadline)?;
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
        } else if let Ok(response) = serde_json::from_slice::<ErrorResponse>(&output.stderr) {
            Err(HerdrCommandError {
                code: response.error.code,
                message: response.error.message,
            }
            .into())
        } else {
            Err(
                std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned())
                    .into(),
            )
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
    TabFocused {
        tab_id: String,
    },
    TabRenamed {
        tab_id: String,
    },
    PaneMoved {
        previous_tab_id: String,
        pane: EventPane,
    },
    PaneClosed {
        workspace_id: String,
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
struct EventPane {
    pane_id: String,
    tab_id: String,
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
    agent_status: AgentStatus,
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

    fn tab_ids(&self) -> Vec<String> {
        self.state
            .sessions
            .get(&self.session)
            .into_iter()
            .flat_map(|tabs| tabs.keys().cloned())
            .collect()
    }

    fn tab_ids_in_workspace(&self, workspace_id: &str) -> Vec<String> {
        let prefix = format!("{workspace_id}:");
        self.tab_ids()
            .into_iter()
            .filter(|tab_id| tab_id.starts_with(&prefix))
            .collect()
    }

    fn save_tab(&mut self, tab_id: &str, state: TabState) -> Result<()> {
        if self.get(tab_id) == Some(&state) {
            return Ok(());
        }
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

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug)]
struct HerdrCommandError {
    code: String,
    message: String,
}

impl fmt::Display for HerdrCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for HerdrCommandError {}

fn is_tab_not_found(error: &(dyn Error + 'static)) -> bool {
    error
        .downcast_ref::<HerdrCommandError>()
        .is_some_and(|error| error.code == "tab_not_found")
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
            "✓ Fix OAuth callback"
        );
        assert!(fixture.state().contains("tracking"));
    }

    #[test]
    fn custom_label_is_pinned_but_receives_status() {
        let fixture = Fixture::new("auth", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();

        assert_eq!(fs::read_to_string(&fixture.renamed).unwrap(), "✓ auth");
        assert!(fixture.state().contains("pinned"));
    }

    #[test]
    fn authoritative_tab_status_wins_over_the_event_payload() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.set_status("blocked");
        fixture.runtime.run_event(STATUS_EVENT).unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "◉ Fix OAuth callback"
        );
    }

    #[test]
    fn status_changes_preserve_the_base_title() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.set_status("blocked");
        fixture.runtime.run_event(STATUS_EVENT).unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "◉ Fix OAuth callback"
        );
        let state = fixture.state();
        assert!(state.contains("Fix OAuth callback"));
        assert!(!state.contains('◉'));
    }

    #[test]
    fn tab_focus_refreshes_done_to_idle_state() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.set_status("done");
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.set_status("idle");
        fixture
            .runtime
            .run_event(
                r#"{"event":"tab.focused","data":{"type":"tab_focused","tab_id":"w1:t1","workspace_id":"w1"}}"#,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "✓ Fix OAuth callback"
        );
    }

    #[test]
    fn manual_rename_updates_the_pinned_base_label() {
        let fixture = Fixture::new("auth", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fs::write(&fixture.tab_label, "billing").unwrap();
        fixture.set_status("blocked");
        fixture
            .runtime
            .run_event(
                r#"{"event":"tab.renamed","data":{"type":"tab_renamed","tab_id":"w1:t1","workspace_id":"w1","label":"billing"}}"#,
            )
            .unwrap();

        assert_eq!(fs::read_to_string(&fixture.renamed).unwrap(), "◉ billing");
        assert!(fixture.state().contains("billing"));
    }

    #[test]
    fn startup_reconciles_restored_status_icons() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.set_status("unknown");
        fixture.runtime.reconcile_startup().unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "○ Fix OAuth callback"
        );
    }

    #[test]
    fn startup_prunes_tabs_confirmed_missing() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fs::remove_file(&fixture.tab_exists).unwrap();

        fixture.runtime.reconcile_startup().unwrap();

        assert!(!fixture.state().contains("w1:t1"));
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
    fn multi_pane_tab_keeps_its_title_but_receives_aggregate_status() {
        let fixture = Fixture::new_with_panes("1", "Fix OAuth callback", false, 2);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        assert_eq!(fs::read_to_string(&fixture.renamed).unwrap(), "✓ 1");
    }

    #[test]
    fn pane_close_refreshes_the_remaining_tab_status() {
        let fixture = Fixture::new_with_panes("1", "Fix OAuth callback", false, 2);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        fixture.set_status("done");
        fixture
            .runtime
            .run_event(
                r#"{"event":"pane.closed","data":{"type":"pane_closed","pane_id":"w1:p2","workspace_id":"w1"}}"#,
            )
            .unwrap();

        assert_eq!(fs::read_to_string(&fixture.renamed).unwrap(), "● 1");
    }

    #[test]
    fn pane_move_refreshes_the_destination_before_the_source() {
        let fixture = Fixture::new("1", "Fix OAuth callback", false);
        fixture.runtime.run_event(STATUS_EVENT).unwrap();
        Store::open(
            &fixture.runtime.state_dir,
            &fixture.runtime.socket_path,
            Instant::now() + EVENT_TIMEOUT,
        )
        .unwrap()
        .save_tab(
            "w1:t2",
            TabState::Tracking {
                labels: ["1", "Fix OAuth callback"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
        )
        .unwrap();
        fs::write(&fixture.command_log, "").unwrap();
        fixture.set_status("blocked");
        fixture
            .runtime
            .run_event(
                r#"{"event":"pane.moved","data":{"type":"pane_moved","previous_tab_id":"w1:t2","pane":{"pane_id":"w1:p1","tab_id":"w1:t1"}}}"#,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&fixture.renamed).unwrap(),
            "◉ Fix OAuth callback"
        );
        let commands = fs::read_to_string(&fixture.command_log).unwrap();
        let destination = commands.find("tab:get:w1:t1").unwrap();
        let source = commands.find("tab:get:w1:t2").unwrap();
        assert!(destination < source);
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
        tab_status: PathBuf,
        tab_exists: PathBuf,
        command_log: PathBuf,
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
            let tab_status_path = directory.path().join("tab-status");
            let tab_exists_path = directory.path().join("tab-exists");
            let command_log_path = directory.path().join("commands");
            fs::write(&tab_label_path, tab_label).unwrap();
            fs::write(&tab_status_path, "idle").unwrap();
            fs::write(&tab_exists_path, "").unwrap();
            fs::write(&command_log_path, "").unwrap();
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
printf '%s:%s:%s\n' "$1" "$2" "$3" >> '{}'
case "$1:$2" in
  pane:get) printf '%s\n' '{{"result":{{"pane":{{"agent":"opencode","tab_id":"w1:t1","terminal_title_stripped":"{title}"}}}}}}' ;;
  tab:get)
    if [ ! -f '{}' ]; then
      printf '%s\n' '{{"id":"test","error":{{"code":"tab_not_found","message":"tab not found"}}}}' >&2
      exit 1
    fi
    printf '{{"result":{{"tab":{{"label":"%s","pane_count":%s,"agent_status":"%s"}}}}}}\n' "$(cat '{}')" '{pane_count}' "$(cat '{}')"
    ;;
  tab:rename) {rename} ;;
esac
"#,
                    command_log_path.display(),
                    tab_exists_path.display(),
                    tab_label_path.display(),
                    tab_status_path.display()
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
                tab_status: tab_status_path,
                tab_exists: tab_exists_path,
                command_log: command_log_path,
                _directory: directory,
            }
        }

        fn state(&self) -> String {
            fs::read_to_string(self.runtime.state_dir.join("state.json")).unwrap()
        }

        fn set_status(&self, status: &str) {
            fs::write(&self.tab_status, status).unwrap();
        }
    }
}
