package server

import "sync"

// waiters lets a request wait for a worker's desired set to change.
//
// Each worker has a channel that is closed when it changes and replaced with
// a fresh one. A waiter takes the channel before reading the version, so a
// change that lands between the read and the wait still wakes it.
type waiters struct {
	mu    sync.Mutex
	chans map[string]chan struct{}
}

func newWaiters() *waiters {
	return &waiters{chans: map[string]chan struct{}{}}
}

// watch returns a channel that is closed at the next change to workerID.
func (w *waiters) watch(workerID string) <-chan struct{} {
	w.mu.Lock()
	defer w.mu.Unlock()
	ch, ok := w.chans[workerID]
	if !ok {
		ch = make(chan struct{})
		w.chans[workerID] = ch
	}
	return ch
}

func (w *waiters) wake(workerID string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if ch, ok := w.chans[workerID]; ok {
		close(ch)
		delete(w.chans, workerID)
	}
}

func (w *waiters) wakeAll() {
	w.mu.Lock()
	defer w.mu.Unlock()
	for id, ch := range w.chans {
		close(ch)
		delete(w.chans, id)
	}
}
