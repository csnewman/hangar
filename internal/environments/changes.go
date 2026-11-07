package environments

import "sync"

// Changes passes Channel's notifications on to everyone watching on this
// replica.
type Changes struct {
	mu       sync.Mutex
	watchers map[*Watch]struct{}
}

func NewChanges() *Changes { return &Changes{watchers: map[*Watch]struct{}{}} }

// Publish passes on that an environment changed. An empty ID is that any
// may have, as after notifications were lost.
func (c *Changes) Publish(id string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for w := range c.watchers {
		w.add(id)
	}
}

// Watch collects the changes published after it is called, until it is
// closed.
func (c *Changes) Watch() *Watch {
	w := &Watch{c: c, ready: make(chan struct{}, 1), ids: map[string]bool{}}
	c.mu.Lock()
	c.watchers[w] = struct{}{}
	c.mu.Unlock()
	return w
}

// Watch is one watcher's changes, as yet untaken.
type Watch struct {
	c     *Changes
	ready chan struct{}

	mu  sync.Mutex
	ids map[string]bool
	all bool
}

func (w *Watch) add(id string) {
	w.mu.Lock()
	if id == "" {
		w.all = true
	} else {
		w.ids[id] = true
	}
	w.mu.Unlock()
	select {
	case w.ready <- struct{}{}:
	default:
	}
}

// Ready receives when there are changes to take.
func (w *Watch) Ready() <-chan struct{} { return w.ready }

// Take returns the environments changed since it was last called, and
// whether any may have.
func (w *Watch) Take() (ids []string, all bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for id := range w.ids {
		ids = append(ids, id)
	}
	clear(w.ids)
	all, w.all = w.all, false
	return ids, all
}

// Close stops collecting.
func (w *Watch) Close() {
	w.c.mu.Lock()
	delete(w.c.watchers, w)
	w.c.mu.Unlock()
}
