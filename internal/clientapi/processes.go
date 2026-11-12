package clientapi

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/tunnel"
)

// procsRequest asks a running environment's agent about its processes,
// through its worker.
func (h *handler) procsRequest(ctx context.Context, id string, req procs.Request) (api.Environment, procs.Reply, error) {
	env, err := h.envs.Get(ctx, principal(ctx), id)
	if err != nil {
		return env, procs.Reply{}, err
	}
	if env.Phase != api.PhaseRunning || env.WorkerID == "" {
		return env, procs.Reply{}, errNotRunning
	}
	if h.tunnels == nil {
		return env, procs.Reply{}, errUnreachable
	}
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindProcesses, Environment: env.ID})
	if err != nil {
		h.log.Warn("opening a processes stream", "environment", env.ID, "err", err)
		return env, procs.Reply{}, errUnreachable
	}
	defer stream.Close()
	stream.SetDeadline(time.Now().Add(15 * time.Second))
	b, _ := json.Marshal(req)
	if _, err := stream.Write(append(b, '\n')); err != nil {
		return env, procs.Reply{}, errUnreachable
	}
	line, err := bufio.NewReader(stream).ReadBytes('\n')
	if err != nil {
		return env, procs.Reply{}, errUnreachable
	}
	var reply procs.Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		return env, procs.Reply{}, fmt.Errorf("reading the process list: %w", err)
	}
	return env, reply, nil
}

func (h *handler) ListProcesses(ctx context.Context, req ListProcessesRequestObject) (ListProcessesResponseObject, error) {
	_, reply, err := h.procsRequest(ctx, req.ID, procs.Request{Op: "list"})
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return ListProcesses404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errNotRunning):
		return ListProcesses409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errUnreachable):
		return ListProcesses503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	if reply.Error != "" {
		return ListProcesses503JSONResponse{UnavailableJSONResponse{Error: reply.Error}}, nil
	}
	out := ListProcesses200JSONResponse{CPUs: reply.CPUs, MemoryBytes: reply.MemoryBytes, Processes: make([]Process, len(reply.Processes))}
	for i, p := range reply.Processes {
		out.Processes[i] = Process{Pid: p.PID, Ppid: p.PPID, User: p.User, Name: p.Name, Command: p.Command,
			State: p.State, CPUPercent: float32(p.CPUPercent), RssBytes: p.RSSBytes, Threads: p.Threads,
			Started: p.Started}
		if p.Protected {
			protected := true
			out.Processes[i].Protected = &protected
		}
	}
	return out, nil
}

func (h *handler) SignalProcess(ctx context.Context, req SignalProcessRequestObject) (SignalProcessResponseObject, error) {
	signal := string(req.Body.Signal)
	if signal != "TERM" && signal != "KILL" {
		return SignalProcess400JSONResponse{InvalidJSONResponse{Error: "the signal is TERM or KILL"}}, nil
	}
	env, reply, err := h.procsRequest(ctx, req.ID, procs.Request{Op: "kill", PID: req.Pid, Signal: signal})
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SignalProcess404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errNotRunning):
		return SignalProcess409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errUnreachable):
		return SignalProcess503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	h.access(ctx, env, "environment.signal_process", map[string]any{"pid": req.Pid, "signal": signal, "error": reply.Error})
	if reply.Error != "" {
		return SignalProcess409JSONResponse{ConflictJSONResponse{Error: reply.Error}}, nil
	}
	return SignalProcess204Response{}, nil
}

// ListActivity is the audit log's events about an environment, as its
// page in the web UI shows them: a user who is not an admin sees what is
// theirs, about them, or by them.
func (h *handler) ListActivity(ctx context.Context, req ListActivityRequestObject) (ListActivityResponseObject, error) {
	p := principal(ctx)
	env, err := h.envs.Get(ctx, p, req.ID)
	if errors.Is(err, environments.ErrNotFound) {
		return ListActivity404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	}
	if err != nil {
		return nil, err
	}
	if h.audit == nil {
		return ListActivity200JSONResponse{}, nil
	}
	q := audit.Query{Subjects: []string{"environment:" + env.ID}}
	if req.Params.Before != nil {
		q.Before = *req.Params.Before
	}
	if req.Params.Limit != nil {
		q.Limit = *req.Params.Limit
	}
	if !p.Admin {
		q.AnyOf = []string{"owner:" + p.UserID, "user:" + p.UserID, "actor:" + p.UserID}
	}
	entries, err := h.audit.List(ctx, q)
	if err != nil {
		return nil, err
	}
	out := make(ListActivity200JSONResponse, len(entries))
	for i, e := range entries {
		ev := ActivityEvent{ID: e.ID, At: e.At, Action: e.Action, Details: e.Details}
		ev.Actor.Kind, ev.Actor.Name = e.ActorKind, e.ActorName
		if id := e.ActorUser + e.ActorWorker; id != "" {
			ev.Actor.ID = &id
		}
		ev.Target.Type, ev.Target.ID, ev.Target.Name = e.Target.Type, e.Target.ID, e.Target.Name
		if ev.Details == nil {
			ev.Details = map[string]any{}
		}
		out[i] = ev
	}
	return out, nil
}

// stats is what a running environment is using, as its worker last said.
func stats(s *api.EnvironmentStats) *EnvironmentStats {
	if s == nil {
		return nil
	}
	f := func(v float64) *float32 { x := float32(v); return &x }
	i := func(v int) *int { return &v }
	disk := s.DiskUsedBytes
	return &EnvironmentStats{
		CPUPercent:      float32(s.CPUPercent),
		MemoryUsedMiB:   s.MemoryUsedMiB,
		MemoryTotalMiB:  s.MemoryTotalMiB,
		MemoryCachedMiB: i(s.MemoryCachedMiB),
		HostMemoryMiB:   i(s.HostMemoryMiB),
		SwapTotalMiB:    i(s.SwapTotalMiB),
		SwapUsedMiB:     i(s.SwapUsedMiB),
		DiskUsedBytes:   &disk,
		DiskReadBps:     f(s.DiskReadBps),
		DiskWriteBps:    f(s.DiskWriteBps),
		NetRxBps:        f(s.NetRxBps),
		NetTxBps:        f(s.NetTxBps),
	}
}
