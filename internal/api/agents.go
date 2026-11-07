package api

import "time"

// Task is what an environment is for at the moment: a line its coding
// agent sets as it works, or its owner does.
type Task struct {
	ID   string `json:"id"`
	Text string `json:"text"`
	// SetBy is TaskByAgent or TaskByUser.
	SetBy string    `json:"set_by"`
	SetAt time.Time `json:"set_at"`
	// DoneAt is when it was marked done, or replaced by another; nil for
	// the environment's task.
	DoneAt *time.Time `json:"done_at,omitempty"`
}

// Who set a task.
const (
	TaskByAgent = "agent"
	TaskByUser  = "user"
)

// MaxTaskLength is the most characters a task may have.
const MaxTaskLength = 200

// AgentState is what a coding agent's session is doing.
type AgentState string

const (
	// AgentAsking wants a decision: a permission prompt, a question.
	AgentAsking AgentState = "asking"
	// AgentFinished ended its turn, and its owner has not looked since.
	AgentFinished AgentState = "finished"
	// AgentWorking has a prompt or a tool running.
	AgentWorking AgentState = "working"
	// AgentIdle is waiting, and has been seen.
	AgentIdle AgentState = "idle"
	// AgentEnded is a session whose agent has exited.
	AgentEnded AgentState = "ended"
)

// The coding agents whose sessions are reported.
const (
	AgentClaude = "claude"
	AgentCodex  = "codex"
)

// AgentSession is one coding agent's session in an environment, as the
// agent's hooks report it. Only what it is doing travels: the tool and the
// file, never prompts, file contents or commands.
type AgentSession struct {
	// ID is the agent's own ID for the session.
	ID string `json:"id"`
	// Agent is AgentClaude or AgentCodex.
	Agent string `json:"agent"`
	// Terminal is the terminal session it runs in, when it runs in one.
	Terminal string     `json:"terminal,omitempty"`
	State    AgentState `json:"state"`
	// Step is what a working session is doing ("Edit src/main.go"), and
	// Message what an asking one asks, in the agent's words.
	Step    string `json:"step,omitempty"`
	Message string `json:"message,omitempty"`
	// Since is when it took its state.
	Since time.Time `json:"since"`
	// Seen is whether its owner has looked at it since: an asking or
	// finished session they have seen does not need them.
	Seen bool `json:"seen,omitempty"`
}

// NeedsYou is whether a session wants its owner: it asks or has finished,
// and they have not looked since.
func (s AgentSession) NeedsYou() bool {
	return !s.Seen && (s.State == AgentAsking || s.State == AgentFinished)
}
