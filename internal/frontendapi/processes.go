package frontendapi

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/tunnel"
)

// procsRequest asks a running environment about its processes.
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
			State: p.State, CPUPercent: float32(p.CPUPercent), RssBytes: p.RSSBytes, Threads: p.Threads, Started: p.Started, Protected: optionalBool(p.Protected)}
	}
	return out, nil
}

func (h *handler) SignalProcess(ctx context.Context, req SignalProcessRequestObject) (SignalProcessResponseObject, error) {
	signal := string(req.Body.Signal)
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
		return SignalProcess400JSONResponse{InvalidJSONResponse{Error: reply.Error}}, nil
	}
	return SignalProcess204Response{}, nil
}
