package agentstatus

import (
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// endedKept is how long an ended session is kept, so a session that
// finished as it exited still shows it did.
const endedKept = 10 * time.Minute

// maxSessions is the most sessions kept: the oldest ended go first, then
// the oldest.
const maxSessions = 32

// Tracker keeps each coding agent session's state from its hooks.
type Tracker struct {
	mu       sync.Mutex
	sessions map[string]*tracked
}

type tracked struct {
	api.AgentSession
	// pid is the session's agent process, to end it when the process goes
	// without saying so.
	pid int
}

func NewTracker() *Tracker { return &Tracker{sessions: map[string]*tracked{}} }

// Apply takes a hook's event, from process pid (the agent's, or 0 when it
// is not known), at now. It reports whether any session changed.
func (t *Tracker) Apply(h Hook, pid int, now time.Time) bool {
	if h.Session == "" || h.Agent == "" {
		return false
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	s := t.sessions[h.Session]
	if s == nil {
		if h.Event == EventSessionEnd {
			return false
		}
		s = &tracked{AgentSession: api.AgentSession{ID: h.Session, Agent: h.Agent, State: api.AgentIdle, Since: now}}
		t.sessions[h.Session] = s
		t.trim()
	}
	before := *s
	if pid != 0 {
		s.pid = pid
	}
	if h.Terminal != "" {
		s.Terminal = h.Terminal
	}
	state, step, message := s.State, s.Step, s.Message
	switch h.Event {
	case EventSessionStart:
		// A session compacted or resumed carries on as it was.
		if before.State == api.AgentEnded {
			state, step, message = api.AgentIdle, "", ""
		}
	case EventUserPromptSubmit:
		state, step, message = api.AgentWorking, "", ""
	case EventPreToolUse:
		if !IsSetTask(h.Tool) {
			state, step, message = api.AgentWorking, h.Step, ""
		}
	case EventPostToolUse:
		state, message = api.AgentWorking, ""
	case EventPermissionRequest:
		state, step, message = api.AgentAsking, h.Step, "Permission to use "+h.Tool
	case EventNotification:
		if asks(h) {
			state, message = api.AgentAsking, h.Message
		}
	case EventStop:
		state, step, message = api.AgentFinished, "", ""
	case EventSessionEnd:
		state, step, message = api.AgentEnded, "", ""
	}
	if state != s.State {
		s.Since = now
	}
	s.State, s.Step, s.Message = state, step, message
	return s.AgentSession != before.AgentSession
}

// asks is whether a notification wants a decision: a permission prompt or a
// question, not the reminder that the agent is waiting for its next
// prompt.
func asks(h Hook) bool {
	switch h.Notification {
	case "permission_prompt", "elicitation_dialog", "elicitation_url_dialog", "agent_needs_input":
		return true
	case "":
		return h.Message != "" && !strings.Contains(h.Message, "waiting for your input")
	}
	return false
}

// trim keeps at most maxSessions, the oldest ended going first.
func (t *Tracker) trim() {
	for len(t.sessions) > maxSessions {
		rank := func(s *tracked) int {
			if s.State == api.AgentEnded {
				return 0
			}
			return 1
		}
		var oldest *tracked
		for _, s := range t.sessions {
			if oldest == nil || rank(s) < rank(oldest) || rank(s) == rank(oldest) && s.Since.Before(oldest.Since) {
				oldest = s
			}
		}
		delete(t.sessions, oldest.ID)
	}
}

// Sweep ends the sessions whose agent process has gone, as alive says, and
// forgets those ended long enough ago. It reports whether any changed.
func (t *Tracker) Sweep(now time.Time, alive func(pid int) bool) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	changed := false
	for id, s := range t.sessions {
		switch {
		case s.State == api.AgentEnded && now.Sub(s.Since) > endedKept:
			delete(t.sessions, id)
			changed = true
		case s.State != api.AgentEnded && s.pid != 0 && !alive(s.pid):
			s.State, s.Step, s.Message, s.Since = api.AgentEnded, "", "", now
			changed = true
		}
	}
	return changed
}

// Sessions are the sessions, oldest first.
func (t *Tracker) Sessions() []api.AgentSession {
	t.mu.Lock()
	defer t.mu.Unlock()
	out := make([]api.AgentSession, 0, len(t.sessions))
	for _, s := range t.sessions {
		out = append(out, s.AgentSession)
	}
	slices.SortFunc(out, func(a, b api.AgentSession) int {
		if c := a.Since.Compare(b.Since); c != 0 {
			return c
		}
		return strings.Compare(a.ID, b.ID)
	})
	return out
}
