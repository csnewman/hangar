//! Hangar's Claude Code plugin: the hooks and the MCP server.

use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::event;

/// Where the agent writes the plugin at each boot. `user::PLUGIN_ENV`
/// loads it into every claude started in the guest's sessions, with
/// nothing written to the person's own settings, which the profile
/// carries between environments.
pub const PLUGIN_DIR: &str = "/run/hangar/claude-plugin";

/// The hook events the plugin follows.
const CLAUDE_HOOKS: &[&str] = &[
    event::SESSION_START,
    event::SESSION_END,
    event::USER_PROMPT_SUBMIT,
    event::PRE_TOOL_USE,
    event::POST_TOOL_USE,
    event::PERMISSION_REQUEST,
    event::NOTIFICATION,
    event::STOP,
];

/// Writes the plugin to `dir`: hooks for every event followed, and the MCP
/// server, each running `agent`, this binary.
pub fn write_plugin(dir: &Path, agent: &str) -> io::Result<()> {
    let hook = json!([{"hooks": [{
        "type": "command", "command": format!("{agent} hook claude"), "timeout": 5,
    }]}]);
    let hooks: Map<String, Value> = CLAUDE_HOOKS
        .iter()
        .map(|e| (e.to_string(), hook.clone()))
        .collect();
    let files = [
        (
            ".claude-plugin/plugin.json",
            json!({
                "name": "hangar",
                "description": "Shows Hangar what Claude is doing in this environment, and lets it set the environment's task.",
            }),
        ),
        ("hooks/hooks.json", json!({ "hooks": hooks })),
        (
            ".mcp.json",
            json!({"mcpServers": {"hangar": {"command": agent, "args": ["mcp"]}}}),
        ),
    ];
    for (name, content) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut b = serde_json::to_vec_pretty(&content)?;
        b.push(b'\n');
        std::fs::write(path, b)?;
    }
    Ok(())
}
