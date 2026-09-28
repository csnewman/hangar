package logs

import (
	"bytes"
	"context"
	"log/slog"
	"sync"
)

// How much of the worker's log is kept in memory: the latest of it whole,
// and of each environment's lines.
const (
	WholeSize       = 1 << 20
	EnvironmentSize = 64 << 10
)

// ring keeps the latest bytes written to a log that only grows.
type ring struct {
	buf []byte
	cap int
	// end is how much has ever been written, so the buffer holds what
	// begins at end-len(buf).
	end int64
}

func (r *ring) write(p []byte) {
	r.end += int64(len(p))
	r.buf = append(r.buf, p...)
	if over := len(r.buf) - r.cap; over > 0 {
		// Drop whole lines, so what is kept starts at one.
		if i := bytes.IndexByte(r.buf[over:], '\n'); i >= 0 {
			over += i + 1
		}
		r.buf = append(r.buf[:0], r.buf[over:]...)
	}
}

func (r *ring) read(req Request) Reply {
	first := r.end - int64(len(r.buf))
	start, n := clamp(req, first, r.end)
	from := start - first
	return Reply{Start: start, Size: r.end, Data: bytes.Clone(r.buf[from : from+n])}
}

// Rings keeps the worker's log in memory, whole and per environment: each
// record goes to the whole log and, if it names an environment, to that
// environment's.
type Rings struct {
	mu    sync.Mutex
	whole ring
	envs  map[string]*ring
}

func NewRings() *Rings {
	return &Rings{whole: ring{cap: WholeSize}, envs: map[string]*ring{}}
}

func (r *Rings) write(env string, line []byte) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.whole.write(line)
	if env == "" {
		return
	}
	e, ok := r.envs[env]
	if !ok {
		e = &ring{cap: EnvironmentSize}
		r.envs[env] = e
	}
	e.write(line)
}

// Read reads the worker's whole log, or an environment's lines of it.
func (r *Rings) Read(req Request) Reply {
	r.mu.Lock()
	defer r.mu.Unlock()
	if req.Environment == "" {
		return r.whole.read(req)
	}
	e, ok := r.envs[req.Environment]
	if !ok {
		return Reply{Missing: true}
	}
	return e.read(req)
}

// Forget drops an environment's lines, once it is gone.
func (r *Rings) Forget(env string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.envs, env)
}

// Tee is a handler that passes every record on to next and keeps it in
// rings too: everything at Info and above, and below that what next keeps.
func Tee(next slog.Handler, rings *Rings) slog.Handler {
	return &tee{next: next, rings: rings}
}

type tee struct {
	next  slog.Handler
	rings *Rings
	// ops are the attributes and groups added since the root, replayed
	// onto each record's text so it reads as next would write it.
	ops []func(slog.Handler) slog.Handler
	env string
}

func (t *tee) Enabled(ctx context.Context, l slog.Level) bool {
	return l >= slog.LevelInfo || t.next.Enabled(ctx, l)
}

func (t *tee) Handle(ctx context.Context, r slog.Record) error {
	var buf bytes.Buffer
	var h slog.Handler = slog.NewTextHandler(&buf, &slog.HandlerOptions{Level: slog.LevelDebug})
	for _, op := range t.ops {
		h = op(h)
	}
	env := t.env
	r.Attrs(func(a slog.Attr) bool {
		if a.Key == "environment" {
			env = a.Value.String()
		}
		return true
	})
	if err := h.Handle(ctx, r); err == nil {
		t.rings.write(env, buf.Bytes())
	}
	if t.next.Enabled(ctx, r.Level) {
		return t.next.Handle(ctx, r)
	}
	return nil
}

func (t *tee) WithAttrs(attrs []slog.Attr) slog.Handler {
	c := *t
	c.next = t.next.WithAttrs(attrs)
	c.ops = append(append([]func(slog.Handler) slog.Handler{}, t.ops...),
		func(h slog.Handler) slog.Handler { return h.WithAttrs(attrs) })
	for _, a := range attrs {
		if a.Key == "environment" {
			c.env = a.Value.String()
		}
	}
	return &c
}

func (t *tee) WithGroup(name string) slog.Handler {
	c := *t
	c.next = t.next.WithGroup(name)
	c.ops = append(append([]func(slog.Handler) slog.Handler{}, t.ops...),
		func(h slog.Handler) slog.Handler { return h.WithGroup(name) })
	return &c
}
