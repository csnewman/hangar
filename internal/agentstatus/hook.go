// Package agentstatus follows the coding agents running in an environment
// -- Claude Code, Codex -- through their hooks, and lets them set the
// environment's task.
//
// Each agent's hooks run the guest agent (Hook), which reads the hook's
// input, keeps the little that travels -- the event, the tool and what it
// acts on -- and passes it to the guest agent's Tracker over a socket.
// The Tracker turns events into each session's state, which the guest
// agent reports to the server. The task is set through an MCP server the
// agent offers Claude Code (MCP), whose one tool its hooks let run without
// asking.
package agentstatus

import (
	"encoding/json"
	"path/filepath"
	"strings"
)

// Hook is what one hook said, as much of it as is kept: never the prompt,
// the command or what a file holds.
type Hook struct {
	// Agent is api.AgentClaude or api.AgentCodex.
	Agent string `json:"agent"`
	// Event is the hook's event: PreToolUse, Stop, ...
	Event   string `json:"event"`
	Session string `json:"session"`
	// Terminal is the terminal session the agent runs in, from
	// TerminalEnv.
	Terminal string `json:"terminal,omitempty"`
	// PID is the hook command's parent: the agent, or a shell it ran the
	// hook in.
	PID int `json:"pid,omitempty"`
	// Tool and Step are the tool a tool event is for and what it does
	// ("Edit src/main.go").
	Tool string `json:"tool,omitempty"`
	Step string `json:"step,omitempty"`
	// Message is a notification's text, and Notification its kind.
	Message      string `json:"message,omitempty"`
	Notification string `json:"notification,omitempty"`
	// Source is why a session started: startup, resume, clear, compact.
	Source string `json:"source,omitempty"`
}

// TerminalEnv names the terminal session a process runs in, set by the
// guest agent's terminal manager.
const TerminalEnv = "HANGAR_TERMINAL_SESSION"

// The hook events followed. Codex's are named as Claude Code's.
const (
	EventSessionStart      = "SessionStart"
	EventSessionEnd        = "SessionEnd"
	EventUserPromptSubmit  = "UserPromptSubmit"
	EventPreToolUse        = "PreToolUse"
	EventPostToolUse       = "PostToolUse"
	EventPermissionRequest = "PermissionRequest"
	EventNotification      = "Notification"
	EventStop              = "Stop"
)

// hookInput is the part of a hook's input read.
type hookInput struct {
	SessionID        string         `json:"session_id"`
	HookEventName    string         `json:"hook_event_name"`
	Cwd              string         `json:"cwd"`
	ToolName         string         `json:"tool_name"`
	ToolInput        map[string]any `json:"tool_input"`
	Message          string         `json:"message"`
	NotificationType string         `json:"notification_type"`
	Source           string         `json:"source"`
}

// ParseHook reads a hook's input as agent gives it on standard input.
func ParseHook(agent string, input []byte) (Hook, error) {
	var in hookInput
	if err := json.Unmarshal(input, &in); err != nil {
		return Hook{}, err
	}
	h := Hook{Agent: agent, Event: in.HookEventName, Session: in.SessionID, Tool: in.ToolName,
		Notification: in.NotificationType, Source: in.Source}
	if in.HookEventName == EventNotification {
		h.Message = in.Message
	}
	if in.ToolName != "" {
		h.Step = step(in.ToolName, in.ToolInput, in.Cwd)
	}
	return h, nil
}

// step says what a tool does, in a few words: the file it acts on, or the
// short description the agent gave a command. A command itself is not
// kept.
func step(tool string, input map[string]any, cwd string) string {
	str := func(k string) string {
		s, _ := input[k].(string)
		return strings.Join(strings.Fields(s), " ")
	}
	path := str("file_path")
	if path == "" {
		path = str("notebook_path")
	}
	if path == "" {
		path = str("path")
	}
	if path != "" && cwd != "" && filepath.IsAbs(path) {
		if rel, err := filepath.Rel(cwd, path); err == nil && !strings.HasPrefix(rel, "..") {
			path = rel
		}
	}
	switch tool {
	case "Edit", "MultiEdit", "NotebookEdit":
		return "Edit " + path
	case "Write":
		return "Write " + path
	case "Read":
		return "Read " + path
	case "Bash":
		if d := str("description"); d != "" {
			return d
		}
		return "Run a command"
	case "Grep", "Glob":
		return "Search the code"
	case "WebFetch":
		return "Read a web page"
	case "WebSearch":
		return "Search the web"
	case "Task", "Agent":
		if d := str("description"); d != "" {
			return "Subagent: " + d
		}
		return "Run a subagent"
	case "TodoWrite":
		return "Update the to-do list"
	}
	if parts := strings.Split(tool, "__"); len(parts) == 3 && parts[0] == "mcp" {
		return "Use " + parts[2]
	}
	if path != "" {
		return tool + " " + path
	}
	return tool
}
