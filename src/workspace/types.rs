use serde::{Deserialize, Serialize};

impl InspectorOutput {
    pub fn empty() -> Self {
        Self {
            terminals: vec![],
            tmux: vec![],
            shelldon: vec![],
            zellij: vec![],
            ides: vec![],
            browsers: vec![],
        }
    }

    /// Populate `uri` fields on all leaf items.
    pub fn populate_uris(&mut self) {
        for term in &mut self.terminals {
            let app_lower = term.app.to_lowercase();
            let is_windows_terminal = app_lower == "windows terminal";
            // Use the URI slug for Windows Terminal ("windows-terminal") so that
            // URIs from detect() and locate() share the same format.
            let app_slug = if is_windows_terminal {
                "windows-terminal".to_string()
            } else {
                app_lower
            };
            for win in &mut term.windows {
                for (i, tab) in win.tabs.iter_mut().enumerate() {
                    // For Windows Terminal, use the shell PID as the tab identifier so
                    // that focus remains correct even after the user reorders tabs by drag.
                    let tab_id = if is_windows_terminal {
                        tab.shell_pid
                            .filter(|&p| p != 0)
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| (i + 1).to_string())
                    } else {
                        (i + 1).to_string()
                    };
                    tab.uri = Some(format!(
                        "workspace://{}/window:{}/tab:{}",
                        app_slug, win.id, tab_id
                    ));
                }
            }
        }

        for session in &mut self.tmux {
            for win in &mut session.windows {
                for pane in &mut win.panes {
                    pane.uri = Some(format!(
                        "workspace://tmux:{}/window:{}/pane:{}",
                        session.name, win.index, pane.index
                    ));
                }
            }
        }

        for inst in &mut self.shelldon {
            for pane in &mut inst.panes {
                for tab in &mut pane.tabs {
                    tab.uri = Some(format!(
                        "workspace://shelldon:{}/tab:{}",
                        inst.session_id, tab.tab_id
                    ));
                }
            }
        }

        for session in &mut self.zellij {
            for tab in &mut session.tabs {
                for pane in &mut tab.panes {
                    pane.uri = Some(format!(
                        "workspace://zellij:{}/tab:{}/pane:{}",
                        session.name, tab.position, pane.pane_id
                    ));
                }
            }
        }

        for browser in &mut self.browsers {
            let app = browser.app.to_lowercase().replace(' ', "-");
            for win in &mut browser.windows {
                for tab in &mut win.tabs {
                    tab.uri = Some(format!(
                        "workspace://{}/window:{}/tab:{}",
                        app, win.id, tab.index
                    ));
                }
            }
        }

        for ide in &mut self.ides {
            let app = ide.app.to_lowercase();
            for project in &mut ide.projects {
                project.uri = Some(format!(
                    "workspace://{}/project:{}",
                    app, project.name
                ));
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct InspectorOutput {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<TerminalEmulator>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tmux: Vec<TmuxSession>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shelldon: Vec<ShelldonInstance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub zellij: Vec<ZellijSession>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ides: Vec<IdeInstance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub browsers: Vec<BrowserInstance>,
}

#[derive(Serialize, Deserialize)]
pub struct IdeInstance {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub projects: Vec<IdeProject>,
}

#[derive(Serialize, Deserialize)]
pub struct IdeProject {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub path: String,
    pub active: bool,
}

#[derive(Serialize, Deserialize)]
pub struct BrowserInstance {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub windows: Vec<BrowserWindow>,
}

#[derive(Serialize, Deserialize)]
pub struct BrowserWindow {
    pub id: String,
    pub tabs: Vec<BrowserTab>,
}

#[derive(Serialize, Deserialize)]
pub struct BrowserTab {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalEmulator {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub windows: Vec<TerminalWindow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalWindow {
    pub id: String,
    pub tabs: Vec<TerminalTab>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalTab {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct TmuxSession {
    pub name: String,
    pub id: String,
    pub attached: bool,
    pub windows: Vec<TmuxWindow>,
}

#[derive(Serialize, Deserialize)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub panes: Vec<TmuxPane>,
}

#[derive(Serialize, Deserialize)]
pub struct TmuxPane {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub pid: u32,
    pub command: String,
    pub cwd: String,
    pub width: u32,
    pub height: u32,
    pub active: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ShelldonInstance {
    pub pid: u32,
    pub port: u16,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    pub panes: Vec<ShelldonPane>,
}

#[derive(Serialize, Deserialize)]
pub struct ShelldonPane {
    pub pane_id: u32,
    pub name: String,
    pub is_focused: bool,
    pub tabs: Vec<ShelldonTab>,
}

#[derive(Serialize, Deserialize)]
pub struct ShelldonTab {
    pub tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub title: String,
    pub pane_type: String,
    pub is_active: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ZellijSession {
    pub name: String,
    pub tabs: Vec<ZellijTab>,
}

#[derive(Serialize, Deserialize)]
pub struct ZellijTab {
    pub id: u32,
    pub position: u32,
    pub name: String,
    pub active: bool,
    pub panes: Vec<ZellijPane>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ZellijPane {
    #[serde(skip)]
    pub tab_id: u32,
    pub pane_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub title: String,
    pub command: String,
    pub cwd: String,
    pub columns: u32,
    pub rows: u32,
    pub focused: bool,
}
