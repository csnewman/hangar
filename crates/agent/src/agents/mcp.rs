//! `hangar-agent-rs mcp`: an MCP server on stdio, one JSON-RPC message a
//! line, with one tool, set_task, which sets the environment's task
//! through the agent.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Request, serve::ask};

/// The MCP revision offered to a client that asks for none.
const PROTOCOL: &str = "2025-06-18";

/// The set_task tool's description, which is what tells the model when to
/// use it.
const SET_TASK_DESCRIPTION: &str = "Set the one-line task Hangar shows for this environment, so the person can tell \
     their environments apart and see what you are working on. Call it when you start on a piece of work \
     and whenever the work changes. A few words, like a commit's title: \"Fix rounding in checkout totals\".";

/// How long setting the task may take: the agent waits for a session to the
/// server, then for the server.
const TASK_WAIT: Duration = Duration::from_secs(35);

/// Serves MCP on `input` and `out` until `input` ends, setting the task
/// through the agent on `socket`.
pub fn serve_mcp(input: impl BufRead, mut out: impl Write, socket: &Path) -> io::Result<()> {
    for line in input.split(b'\n') {
        let line = line?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let reply = match serde_json::from_slice::<Value>(&line) {
            Err(_) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"},
            })),
            // A notification, which has no ID, is not answered.
            Ok(req) => req.get("id").filter(|id| !id.is_null()).map(|id| {
                let method = req
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match handle(method, req.get("params").unwrap_or(&Value::Null), socket) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, message)) => json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": code, "message": message},
                    }),
                }
            }),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn handle(method: &str, params: &Value, socket: &Path) -> Result<Value, (i32, &'static str)> {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or(PROTOCOL);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "hangar", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": [{
            "name": "set_task",
            "description": SET_TASK_DESCRIPTION,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The task, in a few words."},
                },
                "required": ["task"],
            },
        }]})),
        "tools/call" => {
            if params.get("name").and_then(Value::as_str) != Some("set_task") {
                return Err((-32602, "no such tool"));
            }
            let text = params
                .pointer("/arguments/task")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty() {
                return Ok(tool_result("The task needs some text.", true));
            }
            let req = Request {
                set_task: Some(text),
                ..Request::default()
            };
            Ok(match ask(socket, &req, TASK_WAIT) {
                Err(e) => tool_result(&format!("Hangar could not set the task: {e}"), true),
                Ok(r) => match r.task {
                    None => tool_result("Hangar has no task for this environment.", false),
                    Some(t) => {
                        tool_result(&format!("The environment's task is \"{}\".", t.text), false)
                    }
                },
            })
        }
        _ => Err((-32601, "method not found")),
    }
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}
