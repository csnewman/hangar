package agentstatus_test

import (
	"bufio"
	"bytes"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/agentstatus"
	"github.com/csnewman/hangar/internal/api"
)

func TestParseHookKeepsWhatTravels(t *testing.T) {
	for _, c := range []struct {
		input string
		want  agentstatus.Hook
	}{
		{`{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/home/dev/app","tool_name":"Edit",
			"tool_input":{"file_path":"/home/dev/app/src/main.go","old_string":"secret","new_string":"x"}}`,
			agentstatus.Hook{Agent: "claude", Event: "PreToolUse", Session: "s", Tool: "Edit", Step: "Edit src/main.go"}},
		{`{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/home/dev/app","tool_name":"Bash",
			"tool_input":{"command":"psql -c 'select secret'","description":"Run the migration"}}`,
			agentstatus.Hook{Agent: "claude", Event: "PreToolUse", Session: "s", Tool: "Bash", Step: "Run the migration"}},
		{`{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}`,
			agentstatus.Hook{Agent: "claude", Event: "PreToolUse", Session: "s", Tool: "Bash", Step: "Run a command"}},
		{`{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/a","tool_name":"Read","tool_input":{"file_path":"/etc/hosts"}}`,
			agentstatus.Hook{Agent: "claude", Event: "PreToolUse", Session: "s", Tool: "Read", Step: "Read /etc/hosts"}},
		{`{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"mcp__github__create_issue","tool_input":{}}`,
			agentstatus.Hook{Agent: "claude", Event: "PreToolUse", Session: "s", Tool: "mcp__github__create_issue",
				Step: "Use create_issue"}},
		{`{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"my secret plan"}`,
			agentstatus.Hook{Agent: "claude", Event: "UserPromptSubmit", Session: "s"}},
		{`{"session_id":"s","hook_event_name":"Notification","message":"Claude needs your permission to use Bash",
			"notification_type":"permission_prompt"}`,
			agentstatus.Hook{Agent: "claude", Event: "Notification", Session: "s",
				Message: "Claude needs your permission to use Bash", Notification: "permission_prompt"}},
		{`{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"all done, the key is hunter2"}`,
			agentstatus.Hook{Agent: "claude", Event: "Stop", Session: "s"}},
	} {
		got, err := agentstatus.ParseHook("claude", []byte(c.input))
		if err != nil {
			t.Fatal(err)
		}
		if got != c.want {
			t.Errorf("%s:\n got %+v\nwant %+v", c.input, got, c.want)
		}
	}
}

func TestTrackerFollowsATurn(t *testing.T) {
	tr := agentstatus.NewTracker()
	now := time.Unix(1000, 0)
	apply := func(h agentstatus.Hook) api.AgentSession {
		t.Helper()
		h.Agent, h.Session = api.AgentClaude, "s"
		now = now.Add(time.Second)
		tr.Apply(h, 42, now)
		list := tr.Sessions()
		if len(list) != 1 {
			t.Fatalf("after %s: %d sessions", h.Event, len(list))
		}
		return list[0]
	}
	if s := apply(agentstatus.Hook{Event: "SessionStart", Terminal: "t1"}); s.State != api.AgentIdle || s.Terminal != "t1" {
		t.Fatalf("started: %+v", s)
	}
	apply(agentstatus.Hook{Event: "UserPromptSubmit"})
	if s := apply(agentstatus.Hook{Event: "PreToolUse", Tool: "Edit", Step: "Edit a.go"}); s.State != api.AgentWorking || s.Step != "Edit a.go" {
		t.Fatalf("editing: %+v", s)
	}
	working := now
	if s := apply(agentstatus.Hook{Event: "PostToolUse", Tool: "Edit"}); !s.Since.Equal(working.Add(-time.Second)) || s.Step != "Edit a.go" {
		t.Fatalf("still working, since it started: %+v", s)
	}
	// Setting the task is not a step.
	if s := apply(agentstatus.Hook{Event: "PreToolUse", Tool: agentstatus.SetTaskTool}); s.Step != "Edit a.go" {
		t.Fatalf("setting the task: %+v", s)
	}
	if s := apply(agentstatus.Hook{Event: "Notification", Notification: "permission_prompt",
		Message: "Claude needs your permission to use Bash"}); s.State != api.AgentAsking || s.Message == "" {
		t.Fatalf("asking: %+v", s)
	}
	if s := apply(agentstatus.Hook{Event: "PostToolUse", Tool: "Bash"}); s.State != api.AgentWorking || s.Message != "" {
		t.Fatalf("answered: %+v", s)
	}
	if s := apply(agentstatus.Hook{Event: "Stop"}); s.State != api.AgentFinished || s.Step != "" {
		t.Fatalf("finished: %+v", s)
	}
	// The reminder that it waits for a prompt is not a question.
	if s := apply(agentstatus.Hook{Event: "Notification", Notification: "idle_prompt",
		Message: "Claude is waiting for your input"}); s.State != api.AgentFinished {
		t.Fatalf("idle reminder: %+v", s)
	}
	if s := apply(agentstatus.Hook{Event: "SessionStart", Source: "compact"}); s.State != api.AgentFinished {
		t.Fatalf("compacted: %+v", s)
	}

	// Its process goes without a SessionEnd: it ends, and is forgotten
	// later.
	if tr.Sweep(now, func(int) bool { return true }) {
		t.Fatal("swept while its process lives")
	}
	if !tr.Sweep(now, func(pid int) bool { return pid != 42 }) || tr.Sessions()[0].State != api.AgentEnded {
		t.Fatalf("after its process went: %+v", tr.Sessions())
	}
	if !tr.Sweep(now.Add(time.Hour), func(int) bool { return false }) || len(tr.Sessions()) != 0 {
		t.Fatalf("an hour later: %+v", tr.Sessions())
	}
	if tr.Apply(agentstatus.Hook{Agent: api.AgentClaude, Session: "x", Event: "SessionEnd"}, 0, now) {
		t.Fatal("a session that ended before it was seen was kept")
	}
}

// uplink stands in for the profile session.
type uplink struct {
	mu      sync.Mutex
	reports [][]api.AgentSession
	task    *api.Task
}

func (u *uplink) ReportAgents(s []api.AgentSession) {
	u.mu.Lock()
	defer u.mu.Unlock()
	u.reports = append(u.reports, s)
}

func (u *uplink) SetTask(text string) (*api.Task, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	u.task = &api.Task{ID: "t", Text: text, SetBy: api.TaskByAgent}
	return u.task, nil
}

func (u *uplink) Task() *api.Task {
	u.mu.Lock()
	defer u.mu.Unlock()
	return u.task
}

func (u *uplink) last() []api.AgentSession {
	u.mu.Lock()
	defer u.mu.Unlock()
	if len(u.reports) == 0 {
		return nil
	}
	return u.reports[len(u.reports)-1]
}

// socket is somewhere short enough for a unix socket.
func socket(t *testing.T) string {
	dir, err := os.MkdirTemp("/tmp", "as")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return filepath.Join(dir, "agents.sock")
}

// A hook reaches the tracker and is reported; a session starting is told
// to set the task, and set_task runs without asking and sets it.
func TestHooksAndTheTaskTool(t *testing.T) {
	sock := socket(t)
	up := &uplink{}
	go agentstatus.Serve(sock, up, agentstatus.Processes{
		Agent: func(p int) int { return p },
		Alive: func(int) bool { return true },
	})
	for i := 0; ; i++ {
		if _, err := os.Stat(sock); err == nil {
			break
		}
		if i > 100 {
			t.Fatal("the socket never appeared")
		}
		time.Sleep(10 * time.Millisecond)
	}

	run := func(input string) string {
		var out bytes.Buffer
		env := func(k string) string {
			if k == agentstatus.TerminalEnv {
				return "t1"
			}
			return ""
		}
		agentstatus.RunHook(api.AgentClaude, strings.NewReader(input), &out, sock, env, os.Getpid())
		return out.String()
	}
	out := run(`{"session_id":"s1","hook_event_name":"SessionStart","source":"startup"}`)
	if !strings.Contains(out, `"additionalContext"`) || !strings.Contains(out, "set_task") ||
		!strings.Contains(out, "no task yet") {
		t.Fatalf("SessionStart's output: %s", out)
	}
	if out := run(`{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"a.go"}}`); out != "" {
		t.Fatalf("an edit's output: %q", out)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if l := up.last(); len(l) == 1 && l[0].Step == "Edit a.go" && l[0].Terminal == "t1" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("reported: %+v", up.last())
		}
		time.Sleep(20 * time.Millisecond)
	}
	out = run(`{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"` + agentstatus.SetTaskTool +
		`","tool_input":{"task":"Fix it"}}`)
	if !strings.Contains(out, `"permissionDecision":"allow"`) {
		t.Fatalf("set_task's PreToolUse output: %s", out)
	}

	// The MCP server: initialise, list, call.
	in := strings.Join([]string{
		`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}`,
		`{"jsonrpc":"2.0","method":"notifications/initialized"}`,
		`{"jsonrpc":"2.0","id":2,"method":"tools/list"}`,
		`{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_task","arguments":{"task":"Fix rounding"}}}`,
		`{"jsonrpc":"2.0","id":4,"method":"resources/list"}`,
	}, "\n") + "\n"
	var mcpOut bytes.Buffer
	if err := agentstatus.ServeMCP(strings.NewReader(in), &mcpOut, sock); err != nil {
		t.Fatal(err)
	}
	var replies []map[string]any
	sc := bufio.NewScanner(&mcpOut)
	for sc.Scan() {
		var m map[string]any
		if err := json.Unmarshal(sc.Bytes(), &m); err != nil {
			t.Fatal(err)
		}
		replies = append(replies, m)
	}
	if len(replies) != 4 {
		t.Fatalf("%d replies: %v", len(replies), replies)
	}
	if v := replies[0]["result"].(map[string]any)["protocolVersion"]; v != "2025-06-18" {
		t.Errorf("initialize: %v", replies[0])
	}
	if tools := replies[1]["result"].(map[string]any)["tools"].([]any); tools[0].(map[string]any)["name"] != "set_task" {
		t.Errorf("tools: %v", tools)
	}
	if r := replies[2]["result"].(map[string]any); r["isError"] != false || up.Task() == nil || up.Task().Text != "Fix rounding" {
		t.Errorf("set_task: %v; task %+v", r, up.Task())
	}
	if replies[3]["error"] == nil {
		t.Errorf("an unknown method: %v", replies[3])
	}

	// With the task set, a session starting is told it.
	if out := run(`{"session_id":"s2","hook_event_name":"SessionStart","source":"startup"}`); !strings.Contains(out, "Fix rounding") {
		t.Fatalf("SessionStart with a task: %s", out)
	}
}

// Without the guest agent, a hook says nothing and does not fail.
func TestHookWithoutTheAgent(t *testing.T) {
	var out bytes.Buffer
	agentstatus.RunHook(api.AgentClaude, strings.NewReader(`{"session_id":"s","hook_event_name":"SessionStart"}`), &out,
		filepath.Join(t.TempDir(), "none.sock"), func(string) string { return "" }, 1)
	if out.Len() != 0 {
		t.Fatalf("output: %q", out.String())
	}
	agentstatus.RunHook(api.AgentClaude, strings.NewReader(`not json`), io.Discard, "/nonexistent", os.Getenv, 1)
}

func TestWritePlugin(t *testing.T) {
	dir := t.TempDir()
	if err := agentstatus.WritePlugin(dir, "/usr/local/bin/hangar-agent"); err != nil {
		t.Fatal(err)
	}
	var plugin struct{ Name string }
	b, err := os.ReadFile(filepath.Join(dir, ".claude-plugin", "plugin.json"))
	if err != nil || json.Unmarshal(b, &plugin) != nil || plugin.Name != "hangar" {
		t.Fatalf("plugin.json: %s %v", b, err)
	}
	var hooks struct {
		Hooks map[string][]struct {
			Hooks []struct{ Type, Command string }
		}
	}
	b, _ = os.ReadFile(filepath.Join(dir, "hooks", "hooks.json"))
	if err := json.Unmarshal(b, &hooks); err != nil {
		t.Fatal(err)
	}
	if c := hooks.Hooks["PreToolUse"][0].Hooks[0].Command; c != "/usr/local/bin/hangar-agent hook claude" {
		t.Fatalf("PreToolUse's command: %q", c)
	}
	var mcp struct {
		MCPServers map[string]struct {
			Command string
			Args    []string
		} `json:"mcpServers"`
	}
	b, _ = os.ReadFile(filepath.Join(dir, ".mcp.json"))
	if err := json.Unmarshal(b, &mcp); err != nil || mcp.MCPServers["hangar"].Args[0] != "mcp" {
		t.Fatalf(".mcp.json: %s %v", b, err)
	}
}
