package agentstatus

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// PluginDir is where the guest agent writes its Claude Code plugin at each
// boot, and PluginEnv what loads it into every claude started in the
// guest's sessions, with nothing written to the person's own settings,
// which the profile carries between environments.
const (
	PluginDir = "/run/hangar/claude-plugin"
	PluginEnv = "CLAUDE_CODE_PLUGIN_DIRS=" + PluginDir
)

// claudeHooks are the hook events the plugin follows.
var claudeHooks = []string{EventSessionStart, EventSessionEnd, EventUserPromptSubmit, EventPreToolUse,
	EventPostToolUse, EventPermissionRequest, EventNotification, EventStop}

// WritePlugin writes the Claude Code plugin to dir: hooks for every event
// followed, and the MCP server, each running agent, the guest agent's
// binary.
func WritePlugin(dir, agent string) error {
	hook := []map[string]any{{"hooks": []map[string]any{{
		"type": "command", "command": agent + " hook " + "claude", "timeout": 5,
	}}}}
	hooks := map[string]any{}
	for _, e := range claudeHooks {
		hooks[e] = hook
	}
	files := map[string]any{
		".claude-plugin/plugin.json": map[string]any{
			"name":        "hangar",
			"description": "Shows Hangar what Claude is doing in this environment, and lets it set the environment's task.",
		},
		"hooks/hooks.json": map[string]any{"hooks": hooks},
		".mcp.json": map[string]any{"mcpServers": map[string]any{
			"hangar": map[string]any{"command": agent, "args": []string{"mcp"}},
		}},
	}
	for name, content := range files {
		b, err := json.MarshalIndent(content, "", "  ")
		if err != nil {
			return err
		}
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(path, append(b, '\n'), 0o644); err != nil {
			return err
		}
	}
	return nil
}
