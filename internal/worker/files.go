package worker

import (
	"context"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/nfs"
)

// filesRefresh is how often an environment's view of the shared files is
// looked up again, so a pack its owner is given reaches it while it runs.
const filesRefresh = 5 * time.Second

// SharedFiles is a runtime that serves environments the files they share
// with others: from root, each environment what view says it may reach.
type SharedFiles interface {
	UseSharedFiles(root string, view func(ctx context.Context, env string) nfs.View)
}

// filesView is an environment's view of the shared files, as the server
// last gave it. Until it has, the environment reaches nothing.
type filesView struct {
	mu     sync.Mutex
	sets   []string
	hidden map[string][]string
}

func (v *filesView) Sets() []string {
	v.mu.Lock()
	defer v.mu.Unlock()
	return slices.Clone(v.sets)
}

// Hidden reports whether a path in a copy is kept from the environment:
// one hidden, or under a hidden directory.
func (v *filesView) Hidden(set, path string) bool {
	v.mu.Lock()
	defer v.mu.Unlock()
	for _, h := range v.hidden[set] {
		if path == h || path == strings.TrimSuffix(h, "/") || strings.HasSuffix(h, "/") && strings.HasPrefix(path, h) {
			return true
		}
	}
	return false
}

func (v *filesView) set(files api.EnvironmentFiles) {
	hidden := map[string][]string{}
	for set, paths := range files.Hidden {
		hidden[set] = slices.Clone(paths)
	}
	v.mu.Lock()
	v.sets, v.hidden = files.Sets, hidden
	v.mu.Unlock()
}

// filesView is env's view of the shared files, kept current until ctx
// ends.
func (w *Worker) filesView(ctx context.Context, env string) nfs.View {
	v := &filesView{}
	refresh := func() {
		files, err := w.client.environmentFiles(ctx, env)
		if err != nil {
			if ctx.Err() == nil {
				w.log.Warn("looking up an environment's shared files", "environment", env, "err", err)
			}
			return
		}
		v.set(files)
	}
	refresh()
	go func() {
		t := time.NewTicker(filesRefresh)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				refresh()
			}
		}
	}()
	return v
}
