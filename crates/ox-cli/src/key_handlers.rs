use crate::types::APPROVAL_OPTIONS;
use crossterm::event::KeyCode;

/// Write an approval response through the broker for the given thread.
pub(crate) async fn send_approval_response(
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    response: &str,
) {
    use structfs_core_store::{Record, Value};

    if let Some(tid) = active_thread_id {
        let path = ox_kernel::oxpath!("threads", tid, "approval", "response");
        let _ = client
            .write(&path, Record::parsed(Value::String(response.to_string())))
            .await;
    }
}

pub(crate) async fn handle_approval_key(
    dialog: &mut crate::event_loop::DialogState,
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    key: KeyCode,
    _modifiers: crossterm::event::KeyModifiers,
) {
    match key {
        // vim navigation
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
            dialog.approval_selected = dialog.approval_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
            if dialog.approval_selected < APPROVAL_OPTIONS.len() - 1 {
                dialog.approval_selected += 1;
            }
        }
        // number keys for direct selection
        KeyCode::Char(c @ '1'..='6') => {
            let idx = (c as u8 - b'1') as usize;
            if idx < APPROVAL_OPTIONS.len() {
                send_approval_response(client, active_thread_id, APPROVAL_OPTIONS[idx].1).await;
                dialog.approval_selected = 0;
            }
        }
        KeyCode::Enter => {
            send_approval_response(
                client,
                active_thread_id,
                APPROVAL_OPTIONS[dialog.approval_selected].1,
            )
            .await;
            dialog.approval_selected = 0;
        }
        // customize — enter customize dialog
        KeyCode::Char('c') | KeyCode::Char('C') => {
            // Read tool and input_preview from the pending approval in broker
            if let Some(tid) = active_thread_id {
                let pending_path =
                    ox_kernel::oxpath!("threads", tid, "approval", "pending");
                if let Ok(Some(record)) = client.read(&pending_path).await {
                    if let Some(structfs_core_store::Value::Map(m)) = record.as_value() {
                        let tool = m
                            .get("tool_name")
                            .and_then(|v| match v {
                                structfs_core_store::Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let input_preview = m
                            .get("input_preview")
                            .and_then(|v| match v {
                                structfs_core_store::Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let args = crate::dialogs::infer_args(&tool, &input_preview);
                        dialog.pending_customize = Some(crate::types::CustomizeState {
                            tool,
                            args,
                            arg_cursor: 0,
                            effect_idx: 0,
                            scope_idx: 0,
                            focus: 0,
                            network_idx: 1, // default: allow
                            fs_rules: vec![crate::types::FsRuleState {
                                path: "$PWD".into(),
                                read: true,
                                write: true,
                                create: true,
                                delete: true,
                                execute: true,
                            }],
                            fs_sub_focus: 0,
                            fs_path_cursor: 0,
                        });
                    }
                }
            }
        }
        // quick keys
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            send_approval_response(client, active_thread_id, "allow_once").await;
            dialog.approval_selected = 0;
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            send_approval_response(client, active_thread_id, "allow_session").await;
            dialog.approval_selected = 0;
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            send_approval_response(client, active_thread_id, "allow_always").await;
            dialog.approval_selected = 0;
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            send_approval_response(client, active_thread_id, "deny_once").await;
            dialog.approval_selected = 0;
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            send_approval_response(client, active_thread_id, "deny_always").await;
            dialog.approval_selected = 0;
        }
        KeyCode::Esc => {
            send_approval_response(client, active_thread_id, "deny_once").await;
            dialog.approval_selected = 0;
        }
        _ => {}
    }
}

pub(crate) async fn handle_customize_key(
    dialog: &mut crate::event_loop::DialogState,
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    key: KeyCode,
) {
    use crate::dialogs::{EFFECTS, NETWORKS, SCOPES};

    let cust = dialog.pending_customize.as_mut().unwrap();
    let total = cust.total_fields();
    match key {
        KeyCode::Esc => {
            dialog.pending_customize.take();
            send_approval_response(client, active_thread_id, "deny_once").await;
        }
        KeyCode::Tab | KeyCode::Down => {
            cust.focus = if cust.focus >= total - 1 {
                0
            } else {
                cust.focus + 1
            };
            cust.arg_cursor = 0;
        }
        KeyCode::BackTab | KeyCode::Up => {
            cust.focus = if cust.focus == 0 {
                total - 1
            } else {
                cust.focus - 1
            };
            cust.arg_cursor = 0;
        }
        KeyCode::Enter => {
            let cust = dialog.pending_customize.take().unwrap();
            // Determine effect and scope, write as string response
            let effect = EFFECTS[cust.effect_idx];
            let scope = SCOPES[cust.scope_idx];
            let response = format!("{effect}_{scope}");
            send_approval_response(client, active_thread_id, &response).await;
        }
        _ => {
            let num_args = cust.args.len();
            let add_f = cust.add_arg_field();
            let effect_f = cust.effect_field();
            let scope_f = cust.scope_field();

            if cust.focus < num_args {
                // Editing an arg pattern
                let pat = &mut cust.args[cust.focus];
                match key {
                    KeyCode::Char(c) => {
                        pat.insert(cust.arg_cursor, c);
                        cust.arg_cursor += 1;
                    }
                    KeyCode::Backspace if cust.arg_cursor > 0 => {
                        cust.arg_cursor -= 1;
                        pat.remove(cust.arg_cursor);
                    }
                    KeyCode::Left => cust.arg_cursor = cust.arg_cursor.saturating_sub(1),
                    KeyCode::Right if cust.arg_cursor < pat.len() => cust.arg_cursor += 1,
                    _ => {}
                }
            } else if cust.focus == add_f && cust.tool == "shell" {
                if matches!(key, KeyCode::Char(' ')) {
                    cust.args.push("*".into());
                    cust.focus = cust.args.len() - 1;
                    cust.arg_cursor = 1;
                }
            } else if cust.focus == effect_f {
                if matches!(
                    key,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ')
                ) {
                    cust.effect_idx = 1 - cust.effect_idx;
                }
            } else if cust.focus == scope_f {
                match key {
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                        cust.scope_idx = (cust.scope_idx + 1) % SCOPES.len();
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        cust.scope_idx = if cust.scope_idx == 0 {
                            SCOPES.len() - 1
                        } else {
                            cust.scope_idx - 1
                        };
                    }
                    _ => {}
                }
            } else if cust.focus == cust.network_field() {
                if matches!(
                    key,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ')
                ) {
                    cust.network_idx = (cust.network_idx + 1) % NETWORKS.len();
                }
            } else if cust.focus >= cust.fs_start()
                && cust.focus < cust.fs_start() + cust.fs_rules.len()
            {
                let idx = cust.focus - cust.fs_start();
                match cust.fs_sub_focus {
                    0 => match key {
                        KeyCode::Char(' ') => cust.fs_sub_focus = 1,
                        KeyCode::Char(c) => {
                            cust.fs_rules[idx].path.insert(cust.fs_path_cursor, c);
                            cust.fs_path_cursor += 1;
                        }
                        KeyCode::Backspace if cust.fs_path_cursor > 0 => {
                            cust.fs_path_cursor -= 1;
                            cust.fs_rules[idx].path.remove(cust.fs_path_cursor);
                        }
                        KeyCode::Left => {
                            cust.fs_path_cursor = cust.fs_path_cursor.saturating_sub(1)
                        }
                        KeyCode::Right if cust.fs_path_cursor < cust.fs_rules[idx].path.len() => {
                            cust.fs_path_cursor += 1;
                        }
                        _ => {}
                    },
                    1..=5 => match key {
                        KeyCode::Char(' ') => match cust.fs_sub_focus {
                            1 => cust.fs_rules[idx].read = !cust.fs_rules[idx].read,
                            2 => cust.fs_rules[idx].write = !cust.fs_rules[idx].write,
                            3 => cust.fs_rules[idx].create = !cust.fs_rules[idx].create,
                            4 => cust.fs_rules[idx].delete = !cust.fs_rules[idx].delete,
                            5 => cust.fs_rules[idx].execute = !cust.fs_rules[idx].execute,
                            _ => {}
                        },
                        KeyCode::Left | KeyCode::Char('h') => {
                            cust.fs_sub_focus = cust.fs_sub_focus.saturating_sub(1);
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            cust.fs_sub_focus = (cust.fs_sub_focus + 1).min(5);
                        }
                        KeyCode::Char('x') => {
                            cust.fs_rules.remove(idx);
                            cust.fs_sub_focus = 0;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            } else if cust.focus == cust.add_fs_field() && matches!(key, KeyCode::Char(' ')) {
                cust.fs_rules.push(crate::types::FsRuleState {
                    path: String::new(),
                    read: true,
                    write: false,
                    create: false,
                    delete: false,
                    execute: false,
                });
                cust.focus = cust.fs_start() + cust.fs_rules.len() - 1;
                cust.fs_sub_focus = 0;
                cust.fs_path_cursor = 0;
            }
        }
    }
}
