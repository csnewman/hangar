package server

import (
	"net/http"
	"strconv"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// desiredHold is the longest a worker's request for its desired set is held
// open. It returns the set regardless when it expires, which is the slow
// timer that repairs anything a lost notification left behind. It is kept
// well under the idle timeouts of common load balancers.
const desiredHold = 25 * time.Second

func (s *Server) registerWorker(w http.ResponseWriter, r *http.Request) {
	if !s.checkBootstrap(r) {
		writeError(w, http.StatusUnauthorized, "invalid bootstrap token")
		return
	}
	var req api.RegisterWorker
	if !readJSON(w, r, &req) {
		return
	}
	cred, err := s.workers.Register(r.Context(), req)
	if err != nil {
		s.fail(w, err)
		return
	}
	s.log.Info("worker registered", "name", req.Name, "id", cred.ID)
	writeJSON(w, http.StatusCreated, cred)
}

// workerDesired answers with the worker's desired set as soon as its version
// is newer than ?after=, or after desiredHold regardless.
func (s *Server) workerDesired(w http.ResponseWriter, r *http.Request) {
	id := workerID(r)
	ctx := r.Context()
	var after int64
	if v := r.URL.Query().Get("after"); v != "" {
		n, err := strconv.ParseInt(v, 10, 64)
		if err != nil {
			writeError(w, http.StatusBadRequest, "after must be an integer")
			return
		}
		after = n
	}

	// Waiting on a worker's desired set is proof it is alive.
	if err := s.workers.Touch(ctx, id); err != nil {
		s.fail(w, err)
		return
	}

	timeout := time.NewTimer(desiredHold)
	defer timeout.Stop()
	for {
		changed := s.waits.watch(id)
		v, err := s.workers.DesiredVersion(ctx, id)
		if err != nil {
			s.fail(w, err)
			return
		}
		if v > after {
			break
		}
		select {
		case <-changed:
			continue
		case <-timeout.C:
		case <-ctx.Done():
			return
		}
		break
	}

	set, err := s.workers.DesiredSet(ctx, id)
	if err != nil {
		s.fail(w, err)
		return
	}
	writeJSON(w, http.StatusOK, set)
}

func (s *Server) workerStatus(w http.ResponseWriter, r *http.Request) {
	var st api.WorkerStatus
	if !readJSON(w, r, &st) {
		return
	}
	if err := s.workers.ReportStatus(r.Context(), workerID(r), st); err != nil {
		s.fail(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
