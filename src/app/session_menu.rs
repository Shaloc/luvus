//! On-demand named-session switcher shared by desktop and mobile chrome.
//!
//! Discovery and server startup are explicit user actions and run on short-lived
//! workers. The idle app loop retains only small display rows and hit geometry.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::App;

pub(super) const NEW_SESSION_ROWS: usize = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSessionDiscovery {
    pub sessions: Vec<crate::session::SessionInfo>,
    pub remote: crate::session::remote::RemoteRegistry,
    pub hosts: Vec<String>,
    pub host_error: Option<String>,
    pub host_status: Vec<crate::session::remote::HostStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSessionRow {
    pub name: String,
    pub running: bool,
    pub current: bool,
    pub remote: Option<crate::session::remote::RemoteSession>,
    pub merged: bool,
}

impl NamedSessionRow {
    pub fn display_name(&self) -> &str {
        self.remote
            .as_ref()
            .map_or(&self.name, |remote| &remote.session)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemotePromptField {
    Host,
    Name,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NamedSessionPrompt {
    Local {
        name: String,
    },
    Remote {
        name: String,
        hosts: Vec<String>,
        host_index: usize,
        focus: RemotePromptField,
    },
}

impl NamedSessionPrompt {
    fn name(&self) -> &str {
        match self {
            Self::Local { name } | Self::Remote { name, .. } => name,
        }
    }

    fn name_mut(&mut self) -> &mut String {
        match self {
            Self::Local { name } | Self::Remote { name, .. } => name,
        }
    }
}

#[derive(Debug)]
pub struct NamedSessionMenu {
    pub generation: u64,
    pub rows: Vec<NamedSessionRow>,
    pub hosts: Vec<String>,
    pub cursor: usize,
    pub scroll: usize,
    pub loading: bool,
    pub prompt: Option<NamedSessionPrompt>,
    pub error: Option<String>,
    pub preparing: bool,
}

#[derive(Debug)]
pub enum NamedSessionOpenError {
    Exists,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NamedSessionPreparedAction {
    Switch(String),
    Merge(crate::session::remote::RemoteSession),
}

impl App {
    fn refresh_named_sessions(&self, generation: u64) {
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let (remote, host_status) = crate::session::remote::discover_hosts();
                let (hosts, host_error) = match crate::session::remote::enabled_hosts() {
                    Ok(hosts) => (hosts, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                Ok(NamedSessionDiscovery {
                    sessions: crate::session::list_sessions().map_err(|err| err.to_string())?,
                    remote,
                    hosts,
                    host_error,
                    host_status,
                })
            })();
            let _ = tx.send(crate::event::AppEvent::NamedSessionsLoaded { generation, result });
        });
    }

    pub fn apply_named_session_stopped(
        &mut self,
        generation: u64,
        name: String,
        result: Result<(), String>,
    ) {
        let Some(current_generation) = self.named_session_menu.as_ref().map(|menu| menu.generation)
        else {
            match result {
                Ok(()) => self.show_toast(format!("stopped {name}")),
                Err(err) => self.show_toast(format!("could not stop {name}: {err}")),
            }
            return;
        };
        if current_generation != generation {
            match result {
                Ok(()) => {
                    self.show_toast(format!("stopped {name}"));
                    self.refresh_named_sessions(current_generation);
                }
                Err(err) => self.show_toast(format!("could not stop {name}: {err}")),
            }
            return;
        }
        // Capture values before mutable borrow for toast.
        let toast = match &result {
            Ok(()) => format!("stopped {name}"),
            Err(_) => String::new(),
        };
        let (gen, had_error) = {
            let menu = self.named_session_menu.as_mut().unwrap();
            match result {
                Ok(()) => {
                    if let Some(pos) = menu.rows.iter().position(|r| r.name == name) {
                        menu.rows.remove(pos);
                        let count = menu.rows.len() + NEW_SESSION_ROWS;
                        if menu.cursor >= count {
                            menu.cursor = count.saturating_sub(1);
                        }
                    }
                    (menu.generation, false)
                }
                Err(err) => {
                    menu.error = Some(err);
                    (0, true)
                }
            }
        };
        if !had_error {
            self.show_toast(toast);
            self.refresh_named_sessions(gen);
        }
    }

    pub fn open_named_session_menu(&mut self) {
        if !self.server_mode {
            self.show_toast(self.catalog.session_open_failed);
            return;
        }
        self.switcher = false;
        self.named_session_generation = self.named_session_generation.wrapping_add(1);
        let generation = self.named_session_generation;
        self.named_session_menu = Some(NamedSessionMenu {
            generation,
            rows: Vec::new(),
            hosts: Vec::new(),
            cursor: 0,
            scroll: 0,
            loading: true,
            prompt: None,
            error: None,
            preparing: false,
        });
        self.refresh_named_sessions(generation);
    }

    pub fn close_named_session_menu(&mut self) {
        self.named_session_generation = self.named_session_generation.wrapping_add(1);
        self.named_session_menu = None;
        self.session_menu = None;
    }

    pub fn apply_named_sessions_loaded(
        &mut self,
        generation: u64,
        result: Result<NamedSessionDiscovery, String>,
    ) {
        let current = crate::session::display_name();
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        if menu.generation != generation {
            return;
        }
        menu.loading = false;
        match result {
            Ok(discovery) => {
                menu.rows = session_rows(&discovery, &current);
                menu.hosts = discovery.hosts;
                menu.error = discovery
                    .host_error
                    .map(|error| format!("{}: {error}", self.catalog.session_no_ssh_hosts));
                menu.cursor = menu
                    .rows
                    .iter()
                    .position(|row| row.current)
                    .map_or(0, |index| index + NEW_SESSION_ROWS);
                self.remote_host_status = discovery.host_status;
                self.remote_registry_generation = self.remote_registry_generation.wrapping_add(1);
                self.apply_remote_registry_loaded(
                    self.remote_registry_generation,
                    discovery.remote,
                );
            }
            Err(error) => {
                menu.error = Some(format!("{}: {error}", self.catalog.session_open_failed))
            }
        }
    }

    pub fn apply_named_session_prepared(
        &mut self,
        generation: u64,
        action: NamedSessionPreparedAction,
        result: Result<(), NamedSessionOpenError>,
    ) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        if menu.generation != generation {
            return;
        }
        menu.preparing = false;
        match result {
            Ok(()) => {
                self.named_session_menu = None;
                self.named_session_generation = self.named_session_generation.wrapping_add(1);
                match action {
                    NamedSessionPreparedAction::Switch(name) => {
                        self.pending_session_switch = Some(name);
                    }
                    NamedSessionPreparedAction::Merge(target) => {
                        if crate::session::remote::process_target().is_none()
                            && crate::session::display_name() == target.session
                        {
                            self.discover_remote_session(target);
                        } else {
                            self.pending_session_switch = Some(target.session);
                        }
                    }
                }
            }
            Err(NamedSessionOpenError::Exists) => {
                menu.error = Some(self.catalog.session_exists.to_string());
            }
            Err(NamedSessionOpenError::Failed(error)) => {
                menu.error = Some(format!("{}: {error}", self.catalog.session_open_failed));
            }
        }
    }

    pub fn named_session_key(&mut self, key: KeyEvent) {
        let prompt_open = self
            .named_session_menu
            .as_ref()
            .is_some_and(|menu| menu.prompt.is_some());
        if prompt_open {
            match key.code {
                KeyCode::Esc => {
                    self.named_session_generation = self.named_session_generation.wrapping_add(1);
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        menu.generation = self.named_session_generation;
                        menu.prompt = None;
                        menu.error = None;
                        menu.preparing = false;
                    }
                }
                KeyCode::Enter => self.submit_named_session_prompt(),
                KeyCode::Tab => {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            cycle_remote_prompt_focus(menu.prompt.as_mut(), 1);
                        }
                    }
                }
                KeyCode::BackTab => {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            cycle_remote_prompt_focus(menu.prompt.as_mut(), -1);
                        }
                    }
                }
                KeyCode::Left | KeyCode::Up => {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            adjust_remote_prompt(menu.prompt.as_mut(), -1);
                            menu.error = None;
                        }
                    }
                }
                KeyCode::Right | KeyCode::Down => {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            adjust_remote_prompt(menu.prompt.as_mut(), 1);
                            menu.error = None;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            if let Some(prompt) = menu.prompt.as_mut() {
                                prompt.name_mut().pop();
                            }
                            menu.error = None;
                        }
                    }
                }
                KeyCode::Char(character)
                    if !super::keys::is_ctrl_chord(key.modifiers) && !character.is_control() =>
                {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        if !menu.preparing {
                            if let Some(prompt) = menu.prompt.as_mut() {
                                if prompt_accepts_text(prompt)
                                    && prompt.name().len() < 64
                                    && is_session_name_character(character)
                                {
                                    prompt.name_mut().push(character);
                                }
                            }
                            menu.error = None;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        let count = self
            .named_session_menu
            .as_ref()
            .map_or(0, |menu| menu.rows.len() + NEW_SESSION_ROWS);
        match key.code {
            KeyCode::Char('r') => self.open_named_session_menu(),
            KeyCode::Esc | KeyCode::Char('q') => self.close_named_session_menu(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(menu) = self.named_session_menu.as_mut() {
                    menu.cursor = menu.cursor.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(menu) = self.named_session_menu.as_mut() {
                    if count > 0 {
                        menu.cursor = (menu.cursor + 1).min(count - 1);
                    }
                }
            }
            KeyCode::Home => {
                if let Some(menu) = self.named_session_menu.as_mut() {
                    menu.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(menu) = self.named_session_menu.as_mut() {
                    menu.cursor = count.saturating_sub(1);
                }
            }
            KeyCode::Enter => {
                let cursor = self
                    .named_session_menu
                    .as_ref()
                    .map_or(0, |menu| menu.cursor);
                self.activate_named_session_row(cursor);
            }
            _ => {}
        }
    }

    pub fn paste_named_session_prompt(&mut self, text: &str) -> bool {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return false;
        };
        let Some(prompt) = menu.prompt.as_mut() else {
            return true;
        };
        if menu.preparing {
            return true;
        }
        for character in text
            .chars()
            .filter(|character| is_session_name_character(*character))
        {
            if !prompt_accepts_text(prompt) || prompt.name().len() >= 64 {
                break;
            }
            prompt.name_mut().push(character);
        }
        menu.error = None;
        true
    }

    pub fn named_session_click(&mut self, column: u16, row: u16) {
        let hit = |rect: ratatui::layout::Rect| {
            column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
        };
        if self.named_session_close_rect.is_some_and(hit) {
            self.close_named_session_menu();
            return;
        }
        if let Some(index) = self
            .named_session_row_rects
            .iter()
            .find(|(_, rect)| hit(*rect))
            .map(|(index, _)| *index)
        {
            self.activate_named_session_row(index);
            return;
        }
        if !self.compact && !self.named_session_menu_rect.is_some_and(hit) {
            self.close_named_session_menu();
        }
    }

    pub fn move_named_session_cursor(&mut self, delta: i32) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        if menu.prompt.is_some() {
            return;
        }
        let count = menu.rows.len() + NEW_SESSION_ROWS;
        menu.cursor =
            (menu.cursor as i32 + delta).clamp(0, count.saturating_sub(1) as i32) as usize;
    }

    fn activate_named_session_row(&mut self, index: usize) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        if menu.loading || menu.preparing {
            return;
        }
        if index == 0 {
            menu.prompt = Some(NamedSessionPrompt::Local {
                name: String::new(),
            });
            menu.error = None;
            return;
        }
        if index == 1 {
            if menu.hosts.is_empty() {
                if menu.error.is_none() {
                    menu.error = Some(self.catalog.session_no_ssh_hosts.to_string());
                }
                return;
            }
            menu.prompt = Some(NamedSessionPrompt::Remote {
                name: String::new(),
                hosts: menu.hosts.clone(),
                host_index: 0,
                focus: RemotePromptField::Name,
            });
            menu.error = None;
            return;
        }
        let Some((name, current, remote)) = menu
            .rows
            .get(index - NEW_SESSION_ROWS)
            .map(|row| (row.name.clone(), row.current, row.remote.clone()))
        else {
            return;
        };
        if current {
            self.close_named_session_menu();
            return;
        }
        if let Some(target) = remote {
            self.prepare_remote_session(target, self.remote_merge_enabled, false);
        } else {
            self.prepare_named_session(name, false);
        }
    }

    fn submit_named_session_prompt(&mut self) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        if menu.preparing {
            return;
        }
        let Some(prompt) = menu.prompt.clone() else {
            return;
        };
        let name = prompt.name().trim().to_string();
        if crate::session::validate_name(&name).is_err() {
            menu.error = Some(self.catalog.session_name_hint.to_string());
            return;
        }
        match prompt {
            NamedSessionPrompt::Local { .. } => self.prepare_named_session(name, true),
            NamedSessionPrompt::Remote {
                hosts, host_index, ..
            } => {
                let Some(host) = hosts.get(host_index) else {
                    if let Some(menu) = self.named_session_menu.as_mut() {
                        menu.error = Some(self.catalog.session_no_ssh_hosts.to_string());
                    }
                    return;
                };
                match crate::session::remote::RemoteSession::new(host, &name) {
                    Ok(target) => {
                        self.prepare_remote_session(target, self.remote_merge_enabled, true)
                    }
                    Err(error) => {
                        if let Some(menu) = self.named_session_menu.as_mut() {
                            menu.error = Some(error);
                        }
                    }
                }
            }
        }
    }

    fn prepare_named_session(&mut self, name: String, must_be_new: bool) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        menu.preparing = true;
        menu.error = None;
        let generation = menu.generation;
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                if must_be_new {
                    let sessions = crate::session::list_sessions()
                        .map_err(|err| NamedSessionOpenError::Failed(err.to_string()))?;
                    if sessions.iter().any(|session| session.name == name) {
                        return Err(NamedSessionOpenError::Exists);
                    }
                }
                crate::session::start_client_session(&name)
                    .map(|_| ())
                    .map_err(NamedSessionOpenError::Failed)
            })();
            let _ = tx.send(crate::event::AppEvent::NamedSessionPrepared {
                generation,
                action: NamedSessionPreparedAction::Switch(name),
                result,
            });
        });
    }

    fn prepare_remote_session(
        &mut self,
        target: crate::session::remote::RemoteSession,
        merge: bool,
        register: bool,
    ) {
        let Some(menu) = self.named_session_menu.as_mut() else {
            return;
        };
        menu.preparing = true;
        menu.error = None;
        let generation = menu.generation;
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                crate::session::remote::verify_remote_version(&target.host)
                    .map_err(NamedSessionOpenError::Failed)?;
                if register {
                    let existing = crate::session::remote::list_host_sessions(&target.host)
                        .map_err(NamedSessionOpenError::Failed)?;
                    if existing
                        .iter()
                        .any(|session| session.name == target.session)
                    {
                        return Err(NamedSessionOpenError::Exists);
                    }
                    // Connecting the control bridge creates the selected remote
                    // server through its normal lifecycle. No binary is copied.
                    crate::session::remote::ensure_session(&target)
                        .map_err(NamedSessionOpenError::Failed)?;
                    crate::session::remote::add_session(target.clone(), merge)
                        .map_err(NamedSessionOpenError::Failed)?;
                }
                if merge {
                    crate::session::start_client_session(&target.session)
                        .map_err(NamedSessionOpenError::Failed)?;
                } else {
                    crate::session::start_client_session(&target.canonical_name())
                        .map_err(NamedSessionOpenError::Failed)?;
                    crate::session::remote::reload_local_session(&target.session)
                        .map_err(NamedSessionOpenError::Failed)?;
                }
                Ok(())
            })();
            let action = if merge {
                NamedSessionPreparedAction::Merge(target)
            } else {
                NamedSessionPreparedAction::Switch(target.canonical_name())
            };
            let _ = tx.send(crate::event::AppEvent::NamedSessionPrepared {
                generation,
                action,
                result,
            });
        });
    }
}

fn session_rows(discovery: &NamedSessionDiscovery, current: &str) -> Vec<NamedSessionRow> {
    let mut rows: Vec<_> = discovery
        .sessions
        .iter()
        .filter(|session| {
            !discovery
                .remote
                .sessions
                .iter()
                .any(|remote| remote.canonical_name() == session.name)
        })
        .map(|session| NamedSessionRow {
            current: session.name == current,
            name: session.name.clone(),
            running: session.running,
            remote: None,
            merged: discovery.remote.merge_enabled(),
        })
        .collect();
    for target in &discovery.remote.sessions {
        if discovery.remote.merge_enabled()
            && rows
                .iter()
                .any(|row| row.name == target.session && row.remote.is_none())
        {
            continue;
        }
        let name = target.canonical_name();
        rows.push(NamedSessionRow {
            current: name == current,
            running: name == current
                || discovery.host_status.iter().any(|host| {
                    host.host == target.host
                        && host
                            .sessions
                            .iter()
                            .any(|session| session.name == target.session && session.running)
                }),
            name,
            remote: Some(target.clone()),
            merged: discovery.remote.merge_enabled(),
        });
    }
    if !rows.iter().any(|row| row.current) && !current.starts_with("remote-") {
        rows.push(NamedSessionRow {
            name: current.to_string(),
            running: true,
            current: true,
            remote: None,
            merged: discovery.remote.merge_enabled(),
        });
    }
    rows.sort_by(|left, right| {
        (
            !left.current,
            left.remote.is_some(),
            !left.running,
            left.display_name().to_ascii_lowercase(),
        )
            .cmp(&(
                !right.current,
                right.remote.is_some(),
                !right.running,
                right.display_name().to_ascii_lowercase(),
            ))
    });
    rows
}

fn prompt_accepts_text(prompt: &NamedSessionPrompt) -> bool {
    matches!(
        prompt,
        NamedSessionPrompt::Local { .. }
            | NamedSessionPrompt::Remote {
                focus: RemotePromptField::Name,
                ..
            }
    )
}

fn cycle_remote_prompt_focus(prompt: Option<&mut NamedSessionPrompt>, delta: i32) {
    let Some(NamedSessionPrompt::Remote { focus, .. }) = prompt else {
        return;
    };
    let index = match focus {
        RemotePromptField::Host => 0,
        RemotePromptField::Name => 1,
    };
    *focus = match (index + delta).rem_euclid(2) {
        0 => RemotePromptField::Host,
        1 => RemotePromptField::Name,
        _ => RemotePromptField::Name,
    };
}

fn adjust_remote_prompt(prompt: Option<&mut NamedSessionPrompt>, delta: i32) {
    let Some(NamedSessionPrompt::Remote {
        hosts,
        host_index,
        focus,
        ..
    }) = prompt
    else {
        return;
    };
    match focus {
        RemotePromptField::Host if !hosts.is_empty() => {
            *host_index = (*host_index as i32 + delta).rem_euclid(hosts.len() as i32) as usize;
        }
        RemotePromptField::Name | RemotePromptField::Host => {}
    }
}

fn is_session_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::{
        session_rows, NamedSessionDiscovery, NamedSessionMenu, NamedSessionPreparedAction,
        NamedSessionPrompt, NamedSessionRow,
    };
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn info(name: &str, running: bool) -> crate::session::SessionInfo {
        let mut info = crate::session::session_info(None);
        info.name = name.to_string();
        info.running = running;
        info
    }

    fn loaded_generation(rx: &Receiver<crate::event::AppEvent>) -> u64 {
        match rx
            .recv_timeout(Duration::from_secs(1))
            .expect("session refresh should complete")
        {
            crate::event::AppEvent::NamedSessionsLoaded { generation, .. } => generation,
            _ => panic!("expected a named-session refresh"),
        }
    }

    fn discovery(sessions: Vec<crate::session::SessionInfo>) -> NamedSessionDiscovery {
        NamedSessionDiscovery {
            sessions,
            remote: crate::session::remote::RemoteRegistry::default(),
            hosts: Vec::new(),
            host_error: None,
            host_status: Vec::new(),
        }
    }

    #[test]
    fn rows_put_current_then_running_then_stopped() {
        let rows = session_rows(
            &discovery(vec![
                info("z-stopped", false),
                info("b-running", true),
                info("active", true),
                info("a-running", true),
            ]),
            "active",
        );
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["active", "a-running", "b-running", "z-stopped"]);
    }

    #[test]
    fn rows_restore_a_missing_current_session() {
        let rows = session_rows(&discovery(vec![info("default", true)]), "other");
        assert_eq!(rows[0].name, "other");
        assert!(rows[0].current);
        assert!(rows[0].running);
    }

    #[test]
    fn loaded_menu_selects_the_current_session_not_new() {
        let _env = crate::persist::test_env("named-session-menu-current");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            generation: 7,
            rows: Vec::new(),
            hosts: Vec::new(),
            cursor: 0,
            scroll: 0,
            loading: true,
            prompt: None,
            error: None,
            preparing: false,
        });
        app.apply_named_sessions_loaded(
            7,
            Ok(discovery(vec![
                info("stopped", false),
                info("default", true),
            ])),
        );
        let menu = app.named_session_menu.as_ref().unwrap();
        assert_eq!(menu.cursor, 2, "two New rows precede the current session");
        assert!(menu.rows[0].current);
    }

    #[test]
    fn keyboard_navigation_supports_vim_edges_and_q() {
        let _env = crate::persist::test_env("named-session-menu-navigation");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            generation: 1,
            rows: vec![
                NamedSessionRow {
                    name: "default".into(),
                    running: true,
                    current: true,
                    remote: None,
                    merged: false,
                },
                NamedSessionRow {
                    name: "review".into(),
                    running: false,
                    current: false,
                    remote: None,
                    merged: false,
                },
            ],
            hosts: Vec::new(),
            cursor: 2,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        app.named_session_key(key(KeyCode::Char('k')));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 1);
        app.named_session_key(key(KeyCode::Char('k')));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 0);
        app.named_session_key(key(KeyCode::End));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 3);
        app.named_session_key(key(KeyCode::Char('j')));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 3);
        app.named_session_key(key(KeyCode::Home));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 0);
        app.named_session_key(key(KeyCode::Char('j')));
        assert_eq!(app.named_session_menu.as_ref().unwrap().cursor, 1);
        app.named_session_key(key(KeyCode::Char('q')));
        assert!(app.named_session_menu.is_none());
    }

    #[test]
    fn q_remains_text_inside_the_new_session_prompt() {
        let _env = crate::persist::test_env("named-session-menu-prompt-q");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            generation: 1,
            rows: Vec::new(),
            hosts: Vec::new(),
            cursor: 0,
            scroll: 0,
            loading: false,
            prompt: Some(NamedSessionPrompt::Local {
                name: String::new(),
            }),
            error: None,
            preparing: false,
        });

        app.named_session_key(key(KeyCode::Char('q')));

        assert_eq!(
            app.named_session_menu
                .as_ref()
                .and_then(|menu| menu.prompt.as_ref())
                .map(NamedSessionPrompt::name),
            Some("q")
        );
    }

    #[test]
    fn stale_preparation_cannot_handoff_after_the_prompt_is_cancelled() {
        let _env = crate::persist::test_env("named-session-menu-stale");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            generation: 4,
            rows: Vec::new(),
            hosts: Vec::new(),
            cursor: 0,
            scroll: 0,
            loading: false,
            prompt: Some(NamedSessionPrompt::Local {
                name: "review".into(),
            }),
            error: None,
            preparing: true,
        });
        app.named_session_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Esc,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        app.apply_named_session_prepared(
            4,
            NamedSessionPreparedAction::Switch("review".into()),
            Ok(()),
        );
        assert!(app.pending_session_switch.is_none());
        assert!(app.named_session_menu.is_some());
    }

    #[test]
    fn global_merge_groups_local_and_remote_default_without_a_prefixed_row() {
        let target = crate::session::remote::RemoteSession::new("dev-207", "default").unwrap();
        let mut remote = crate::session::remote::RemoteRegistry::default();
        remote.sessions.push(target.clone());
        remote.merged_sessions.insert("default".into());
        let rows = session_rows(
            &NamedSessionDiscovery {
                sessions: vec![info("default", true)],
                remote,
                hosts: vec!["dev-207".into()],
                host_error: None,
                host_status: Vec::new(),
            },
            "default",
        );
        assert_eq!(rows[0].name, "default");
        assert!(rows[0].current);
        assert!(rows[0].merged);
        assert_eq!(
            rows.len(),
            1,
            "one logical session must have one menu entry"
        );
        assert_eq!(rows[0].display_name(), "default");
    }

    #[test]
    fn remote_prompt_selects_only_host_and_name_and_keeps_merge_global() {
        let _env = crate::persist::test_env("named-session-menu-remote-prompt");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            generation: 1,
            rows: Vec::new(),
            hosts: vec!["build-box".into(), "dev-207".into()],
            cursor: 1,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        app.named_session_key(key(KeyCode::Enter));
        app.named_session_key(key(KeyCode::Char('a')));
        app.named_session_key(key(KeyCode::Tab));
        app.named_session_key(key(KeyCode::Right));

        let Some(NamedSessionPrompt::Remote {
            name,
            hosts,
            host_index,
            focus,
        }) = app
            .named_session_menu
            .as_ref()
            .and_then(|menu| menu.prompt.as_ref())
        else {
            panic!("remote prompt was not opened");
        };
        assert_eq!(name, "a");
        assert_eq!(hosts[*host_index], "dev-207");
        assert!(!app.remote_merge_enabled);
        assert_eq!(*focus, super::RemotePromptField::Host);
    }

    #[test]
    fn stopped_session_refreshes_a_reopened_menu_with_its_current_generation() {
        let _env = crate::persist::test_env("named-session-stop-reopened-refresh");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            hosts: Vec::new(),
            generation: 8,
            rows: vec![NamedSessionRow {
                name: "review".into(),
                running: true,
                current: false,
                remote: None,
                merged: false,
            }],
            cursor: 1,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        app.apply_named_session_stopped(7, "review".into(), Ok(()));

        assert_eq!(
            app.named_session_menu.as_ref().unwrap().rows.len(),
            1,
            "a stale result must not edit the replacement menu directly"
        );
        assert_eq!(loaded_generation(&rx), 8);
    }

    #[test]
    fn stopped_session_is_removed_and_refreshed_for_the_matching_menu() {
        let _env = crate::persist::test_env("named-session-stop-current-refresh");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            hosts: Vec::new(),
            generation: 5,
            rows: vec![NamedSessionRow {
                name: "review".into(),
                running: true,
                current: false,
                remote: None,
                merged: false,
            }],
            cursor: 1,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        app.apply_named_session_stopped(5, "review".into(), Ok(()));

        assert!(app.named_session_menu.as_ref().unwrap().rows.is_empty());
        assert_eq!(loaded_generation(&rx), 5);
    }

    #[test]
    fn failed_stop_remains_visible_in_the_matching_menu() {
        let _env = crate::persist::test_env("named-session-stop-error");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            hosts: Vec::new(),
            generation: 3,
            rows: vec![NamedSessionRow {
                name: "review".into(),
                running: true,
                current: false,
                remote: None,
                merged: false,
            }],
            cursor: 1,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        app.apply_named_session_stopped(3, "review".into(), Err("server busy".into()));

        let menu = app.named_session_menu.as_ref().unwrap();
        assert_eq!(menu.rows.len(), 1);
        assert_eq!(menu.error.as_deref(), Some("server busy"));
    }

    #[test]
    fn open_session_menu_creating_flag_blocks_stop_until_ready() {
        let _env = crate::persist::test_env("named-session-menu-preparing-guard");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(100, 30, tx).unwrap();
        app.named_session_menu = Some(NamedSessionMenu {
            hosts: Vec::new(),
            generation: 1,
            rows: vec![NamedSessionRow {
                name: "other".into(),
                running: true,
                current: false,
                remote: None,
                merged: false,
            }],
            cursor: 1,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: true,
        });
        // Right-click guard is row.running && !row.current && !menu.preparing.
        let menu = app.named_session_menu.as_ref().unwrap();
        let row = &menu.rows[0];
        assert!(row.running && !row.current);
        assert!(menu.preparing);
        assert!(!(row.running && !row.current && !menu.preparing));
        // Ineligible rows must clear stale menu.
        app.session_menu = Some(crate::app::SessionMenu {
            name: "old".into(),
            anchor: (0, 0),
            items: Vec::new(),
        });
        app.open_session_menu("current".into(), 0, 0, true, true);
        assert!(
            app.session_menu.is_none(),
            "current row must clear stale Stop menu"
        );
        app.session_menu = Some(crate::app::SessionMenu {
            name: "old".into(),
            anchor: (0, 0),
            items: Vec::new(),
        });
        app.open_session_menu("stopped".into(), 0, 0, false, false);
        assert!(
            app.session_menu.is_none(),
            "stopped row must clear stale Stop menu"
        );
        app.open_session_menu("other".into(), 5, 6, true, false);
        assert!(app.session_menu.is_some());
        assert_eq!(app.session_menu.as_ref().unwrap().name, "other");
    }
}
