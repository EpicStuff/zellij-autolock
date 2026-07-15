use std::collections::BTreeMap;
use zellij_tile::prelude::*;
use zellij_tile::shim::list_clients;

struct TabPane {
    tab_pos: usize,
    pane_id: u32,
}

struct State {
    is_enabled: bool,
    permissions_granted: bool,
    lock_trigger_cmds: Vec<String>,
    reaction_seconds: f64,
    timer_scheduled: bool,
    latest_tab_pane: TabPane,
    latest_mode: InputMode,
    latest_running_command: String,
    plugin_locked: bool,
    pending_plugin_mode: Option<InputMode>,
    manual_override: bool,
    mode_initialized: bool,
    print_to_log: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            is_enabled: true,
            permissions_granted: false,
            lock_trigger_cmds: vec!["vim".to_string(), "nvim".to_string()],
            reaction_seconds: 0.3,
            timer_scheduled: false,
            latest_tab_pane: TabPane {
                tab_pos: usize::MAX,
                pane_id: u32::MAX,
            },
            latest_mode: InputMode::Normal,
            latest_running_command: "".to_string(),
            plugin_locked: false,
            pending_plugin_mode: None,
            manual_override: false,
            mode_initialized: false,
            print_to_log: false,
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        request_permission(&[
            // PermissionType::RunCommands,
            PermissionType::ChangeApplicationState,
            PermissionType::ReadApplicationState,
        ]);
        subscribe(&[
            EventType::InputReceived,
            EventType::ListClients,
            EventType::ModeUpdate,
            EventType::PaneUpdate,
            EventType::PermissionRequestResult,
            EventType::TabUpdate,
            EventType::Timer,
        ]);
        if self.permissions_granted {
            hide_self();
        }
        self.load_configuration(configuration);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(permission) => {
                self.permissions_granted = match permission {
                    PermissionStatus::Granted => true,
                    PermissionStatus::Denied => false,
                };
                if self.permissions_granted {
                    hide_self();
                }
            }

            Event::ModeUpdate(mode_info) => {
                let mode = mode_info.mode;
                let is_plugin_change = self.pending_plugin_mode == Some(mode);

                self.latest_mode = mode;
                self.pending_plugin_mode = None;

                if self.mode_initialized
                    && !is_plugin_change
                    && (mode == InputMode::Locked || mode == InputMode::Normal)
                {
                    self.manual_override = true;
                    self.plugin_locked = false;

                    if self.print_to_log {
                        eprintln!(
                            "[autolock] Manual mode change detected: {:?}. Automatic mode changes are paused until the focused command or pane changes.",
                            mode
                        );
                    }
                }

                self.mode_initialized = true;
                self.start_timer();
            }

            Event::InputReceived => {
                self.start_timer();
            }

            Event::TabUpdate(tab_info) => {
                if let Some(tab) = get_focused_tab(&tab_info) {
                    if tab.position != self.latest_tab_pane.tab_pos {
                        self.latest_tab_pane = TabPane {
                            tab_pos: tab.position,
                            pane_id: u32::MAX,
                        };
                        self.reset_manual_override();
                    }
                }
            }

            Event::PaneUpdate(pane_manifest) => {
                let focused_pane =
                    get_focused_pane(self.latest_tab_pane.tab_pos, &pane_manifest).clone();

                if let Some(pane) = focused_pane {
                    if pane.id != self.latest_tab_pane.pane_id {
                        self.latest_tab_pane = TabPane {
                            tab_pos: self.latest_tab_pane.tab_pos,
                            pane_id: pane.id,
                        };
                        self.reset_manual_override();
                        list_clients();
                    }
                }
            }

            Event::ListClients(clients) => {
                if self.is_enabled {
                    if let Some(current_client) = clients.iter().find(|client| {
                        client.is_current_client && !client.running_command.is_empty()
                    }) {
                        let running_command = current_client.running_command.trim().to_string();

                        let mut is_trigger_cmd = false;

                        if running_command != "N/A" {
                            let running_command_exe =
                                running_command.split_whitespace().collect::<Vec<_>>()[0]
                                    .split('/')
                                    .last()
                                    .unwrap_or("")
                                    .to_string();

                            is_trigger_cmd = self.lock_trigger_cmds.contains(&running_command)
                                || self.lock_trigger_cmds.contains(&running_command_exe);

                            if self.print_to_log {
                                eprintln!(
                                    "[autolock] Detected command: `{}`; Executable: `{}`; Is trigger? {}.",
                                    running_command, running_command_exe, is_trigger_cmd,
                                );
                            }
                        } else if self.print_to_log {
                            eprintln!("[autolock] No command detected.");
                        }

                        if running_command != self.latest_running_command {
                            self.latest_running_command = running_command;
                            self.manual_override = false;
                            self.start_timer();
                        }

                        self.check_and_switch_mode(is_trigger_cmd);
                    }
                }
            }

            Event::Timer(_t) => {
                list_clients();
                self.timer_scheduled = false;
            }

            _ => {}
        }
        return false; // No need to render UI.
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if let Some(payload) = pipe_message.payload {
            let action = payload.to_string();

            if action == "enable" {
                self.is_enabled = true;
                if self.print_to_log {
                    eprintln!("[autolock] Enabled");
                }
            } else if action == "disable" {
                self.is_enabled = false;
                if self.print_to_log {
                    eprintln!("[autolock] Disabled");
                }
            } else if action == "toggle" {
                self.is_enabled = !self.is_enabled;
                if self.print_to_log {
                    eprintln!("[autolock] Enabled: {}", self.is_enabled);
                }
            }
        }

        if self.is_enabled {
            list_clients();
            self.start_timer();
        }

        return false; // No need to render UI.
    }

    fn render(&mut self, _rows: usize, _cols: usize) {}
}

impl State {
    fn check_and_switch_mode(&mut self, is_trigger_cmd: bool) {
        if self.manual_override || self.pending_plugin_mode.is_some() {
            return;
        }

        if is_trigger_cmd && self.latest_mode == InputMode::Normal {
            self.switch_mode(InputMode::Locked, true);
        } else if !is_trigger_cmd
            && self.plugin_locked
            && self.latest_mode == InputMode::Locked
        {
            self.switch_mode(InputMode::Normal, false);
        } else if !is_trigger_cmd && self.plugin_locked && self.latest_mode != InputMode::Locked {
            self.plugin_locked = false;
        }
    }

    fn switch_mode(&mut self, mode: InputMode, plugin_locked: bool) {
        self.pending_plugin_mode = Some(mode);
        self.plugin_locked = plugin_locked;

        if self.print_to_log {
            eprintln!(
                "[autolock] Switching to {:?}; plugin owns lock: {}.",
                mode, plugin_locked
            );
        }

        switch_to_input_mode(&mode);
    }

    fn reset_manual_override(&mut self) {
        self.manual_override = false;
        self.latest_running_command.clear();
    }

    fn load_configuration(&mut self, configuration: BTreeMap<String, String>) {
        if let Some(is_enabled) = configuration.get("is_enabled") {
            self.is_enabled = matches!(is_enabled.trim(), "true" | "t" | "y" | "1");
        }
        if let Some(lock_trigger_cmds) = configuration.get("triggers") {
            self.lock_trigger_cmds = lock_trigger_cmds
                .split('|')
                .map(|s| s.trim().to_string())
                .collect();
        }
        if let Some(reaction_seconds) = configuration.get("reaction_seconds") {
            self.reaction_seconds = reaction_seconds.parse::<f64>().unwrap();
        }
        if let Some(print_to_log) = configuration.get("print_to_log") {
            self.print_to_log = matches!(print_to_log.trim(), "true" | "t" | "y" | "1");
        }

        if self.print_to_log {
            eprintln!("[autolock] Configuration loaded.");
            eprintln!("[autolock] Enabled: {}", self.is_enabled);
            eprintln!("[autolock] Trigger commands: {:?}", self.lock_trigger_cmds);
            eprintln!("[autolock] Reaction seconds: {}", self.reaction_seconds);
        }
    }
    fn start_timer(&mut self) {
        if self.is_enabled && !self.timer_scheduled {
            set_timeout(self.reaction_seconds);
            self.timer_scheduled = true;
        }
    }
}
