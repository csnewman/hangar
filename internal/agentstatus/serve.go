package agentstatus

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// Socket is where hooks and the MCP server reach the guest agent. Anyone
// in the guest may: an agent runs as whoever starts it, and the machine,
// not its users, is the boundary.
const Socket = "/run/hangar/agents.sock"

// Request is one line written to the socket.
type Request struct {
	// Hook is a hook's event, from process PID.
	Hook *Hook `json:"hook,omitempty"`
	// SetTask sets the environment's task; empty marks it done.
	SetTask *string `json:"set_task,omitempty"`
}

// Reply answers a Request, on one line: the environment's task as it is
// after, or why the request failed.
type Reply struct {
	Task  *api.Task `json:"task,omitempty"`
	Error string    `json:"error,omitempty"`
}

// Uplink is the guest agent's session with the server (profile.Guest).
type Uplink interface {
	ReportAgents([]api.AgentSession)
	SetTask(text string) (*api.Task, error)
	Task() *api.Task
}

// Processes finds and checks the agents' processes.
type Processes struct {
	// Agent is the agent process a hook's parent belongs to: the parent
	// itself, or what started the shell it is.
	Agent func(parent int) int
	// Alive is whether a process is still running.
	Alive func(pid int) bool
}

const (
	// reportGather is how long a change waits for those close behind it
	// before the sessions are reported: a tool's PreToolUse and
	// PostToolUse often come together.
	reportGather = 300 * time.Millisecond
	// sweepEvery is how often sessions whose process has gone are ended.
	sweepEvery = 10 * time.Second
)

// Serve serves socket, tracking the agents' sessions and reporting them
// through up, until the socket fails.
func Serve(socket string, up Uplink, procs Processes) error {
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return err
	}
	os.Remove(socket)
	ln, err := net.Listen("unix", socket)
	if err != nil {
		return err
	}
	defer ln.Close()
	if err := os.Chmod(socket, 0o666); err != nil {
		return err
	}

	t := NewTracker()
	changed := make(chan struct{}, 1)
	mark := func() {
		select {
		case changed <- struct{}{}:
		default:
		}
	}
	go func() {
		sweep := time.NewTicker(sweepEvery)
		defer sweep.Stop()
		for {
			select {
			case <-changed:
				time.Sleep(reportGather)
			case <-sweep.C:
				if !t.Sweep(time.Now(), procs.Alive) {
					continue
				}
			}
			up.ReportAgents(t.Sessions())
		}
	}()

	for {
		conn, err := ln.Accept()
		if err != nil {
			return err
		}
		go func() {
			defer conn.Close()
			conn.SetDeadline(time.Now().Add(taskWait))
			line, err := bufio.NewReaderSize(conn, 64<<10).ReadBytes('\n')
			if err != nil && !errors.Is(err, io.EOF) {
				return
			}
			var req Request
			if err := json.Unmarshal(line, &req); err != nil {
				return
			}
			var reply Reply
			switch {
			case req.Hook != nil:
				pid := 0
				if req.Hook.PID > 0 {
					pid = procs.Agent(req.Hook.PID)
				}
				if t.Apply(*req.Hook, pid, time.Now()) {
					mark()
				}
				reply.Task = up.Task()
			case req.SetTask != nil:
				task, err := up.SetTask(*req.SetTask)
				if err != nil {
					reply.Error = err.Error()
				}
				reply.Task = task
			}
			json.NewEncoder(conn).Encode(reply)
		}()
	}
}

// taskWait is how long a request may take: setting the task waits for the
// server.
const taskWait = 30 * time.Second

// ask sends one request to the guest agent on socket and returns its reply,
// giving up after wait.
func ask(socket string, req Request, wait time.Duration) (Reply, error) {
	conn, err := net.DialTimeout("unix", socket, wait)
	if err != nil {
		return Reply{}, err
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(wait))
	if err := json.NewEncoder(conn).Encode(req); err != nil {
		return Reply{}, err
	}
	var r Reply
	if err := json.NewDecoder(conn).Decode(&r); err != nil {
		return Reply{}, err
	}
	if r.Error != "" {
		return r, errors.New(r.Error)
	}
	return r, nil
}

// hookWait is how long a hook waits for the guest agent: a hook must never
// hold its agent up.
const hookWait = 2 * time.Second

// SetTaskTool is the MCP tool that sets the task, as Claude Code names it:
// mcp__plugin_<plugin>_<server>__<tool>.
const SetTaskTool = "mcp__plugin_hangar_hangar__set_task"

// IsSetTask is whether a tool is the one that sets the task.
func IsSetTask(tool string) bool { return tool == SetTaskTool }

// RunHook is a hook's command: it reads the hook's input from in, passes
// what it keeps to the guest agent on socket, and writes to out what the
// agent should be told. getenv and parent are the command's environment
// and parent process. Nothing it meets is an error: a hook that failed
// would show in the agent, and it must work whether or not Hangar does.
func RunHook(agent string, in io.Reader, out io.Writer, socket string, getenv func(string) string, parent int) {
	input, err := io.ReadAll(io.LimitReader(in, 4<<20))
	if err != nil {
		return
	}
	h, err := ParseHook(agent, input)
	if err != nil {
		return
	}
	h.Terminal, h.PID = getenv(TerminalEnv), parent
	reply, err := ask(socket, Request{Hook: &h}, hookWait)
	if err != nil {
		return
	}
	switch {
	case agent == api.AgentClaude && h.Event == EventSessionStart:
		writeHookOutput(out, map[string]any{"hookEventName": EventSessionStart,
			"additionalContext": sessionContext(reply.Task)})
	case agent == api.AgentClaude && h.Event == EventPreToolUse && IsSetTask(h.Tool):
		writeHookOutput(out, map[string]any{"hookEventName": EventPreToolUse, "permissionDecision": "allow",
			"permissionDecisionReason": "Hangar's task for this environment"})
	}
}

func writeHookOutput(out io.Writer, specific map[string]any) {
	json.NewEncoder(out).Encode(map[string]any{"hookSpecificOutput": specific})
}

// sessionContext is what a Claude Code session is told as it starts.
func sessionContext(task *api.Task) string {
	var b strings.Builder
	b.WriteString("This machine is a Hangar environment. Hangar shows the person a one-line task for each of " +
		"their environments, so they can tell them apart and see what each is for. When you start on a piece " +
		"of work, and whenever the work changes, set it with the set_task tool of the hangar MCP server: a few " +
		"words, like a commit's title (\"Fix rounding in checkout totals\").")
	if task != nil {
		b.WriteString(" The environment's task is \"" + task.Text + "\"; leave it if the work is the same.")
	} else {
		b.WriteString(" The environment has no task yet.")
	}
	return b.String()
}
