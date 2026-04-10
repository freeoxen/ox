//! Thin JSON-in/JSON-out executor for fs operations.
//!
//! Reads an `ExecCommand` from stdin, performs the operation, and writes
//! an `ExecResult` to stdout.

use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Deserialize)]
struct ExecCommand {
    op: String,
    args: serde_json::Value,
}

#[derive(Serialize)]
struct ExecResult {
    ok: bool,
    value: serde_json::Value,
}

fn main() {
    // We expect --tool-exec as first arg (for future dispatch)
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args[1] != "--tool-exec" {
        let result = ExecResult {
            ok: false,
            value: serde_json::Value::String("expected --tool-exec flag".into()),
        };
        serde_json::to_writer(std::io::stdout(), &result).unwrap();
        std::process::exit(1);
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();

    let cmd: ExecCommand = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => {
            let result = ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("invalid input: {e}")),
            };
            serde_json::to_writer(std::io::stdout(), &result).unwrap();
            std::process::exit(1);
        }
    };

    let result = match cmd.op.as_str() {
        "fs/read" => op_read(&cmd.args),
        "fs/write" => op_write(&cmd.args),
        "fs/edit" => op_edit(&cmd.args),
        other => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("unknown op: {other}")),
        },
    };

    serde_json::to_writer(std::io::stdout(), &result).unwrap();
}

fn op_read(args: &serde_json::Value) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            }
        }
    };

    match std::fs::read_to_string(path) {
        Ok(content) => ExecResult {
            ok: true,
            value: serde_json::Value::String(content),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("read error: {e}")),
        },
    }
}

fn op_write(args: &serde_json::Value) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            }
        }
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'content'".into()),
            }
        }
    };

    // Create parent directories if needed
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ExecResult {
                    ok: false,
                    value: serde_json::Value::String(format!("mkdir error: {e}")),
                };
            }
        }
    }

    match std::fs::write(path, content) {
        Ok(()) => ExecResult {
            ok: true,
            value: serde_json::Value::String("ok".into()),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("write error: {e}")),
        },
    }
}

fn op_edit(args: &serde_json::Value) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            }
        }
    };
    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'old_string'".into()),
            }
        }
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'new_string'".into()),
            }
        }
    };
    let line_start = args.get("line_start").and_then(|v| v.as_u64());

    // Read the file
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("read error: {e}")),
            }
        }
    };

    // Find all occurrences
    let matches: Vec<usize> = content
        .match_indices(old_string)
        .map(|(idx, _)| idx)
        .collect();

    if matches.is_empty() {
        return ExecResult {
            ok: false,
            value: serde_json::Value::String(format!(
                "'old_string' not found in {}",
                path
            )),
        };
    }

    let replacement_idx = if matches.len() == 1 {
        matches[0]
    } else if let Some(hint) = line_start {
        // Use line_start hint to disambiguate: find the match whose
        // 1-based line number equals the hint.
        let hint = hint as usize;
        let mut found = None;
        for &idx in &matches {
            let line_num = content[..idx].chars().filter(|&c| c == '\n').count() + 1;
            if line_num == hint {
                found = Some(idx);
                break;
            }
        }
        match found {
            Some(idx) => idx,
            None => {
                return ExecResult {
                    ok: false,
                    value: serde_json::Value::String(format!(
                        "'old_string' found {} times but none at line {}",
                        matches.len(),
                        hint
                    )),
                }
            }
        }
    } else {
        return ExecResult {
            ok: false,
            value: serde_json::Value::String(format!(
                "'old_string' found {} times — provide line_start to disambiguate",
                matches.len()
            )),
        };
    };

    // Perform the replacement
    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..replacement_idx]);
    result.push_str(new_string);
    result.push_str(&content[replacement_idx + old_string.len()..]);

    match std::fs::write(path, &result) {
        Ok(()) => ExecResult {
            ok: true,
            value: serde_json::Value::String("ok".into()),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("write error: {e}")),
        },
    }
}
