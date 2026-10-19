package logs

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"slices"
	"sync"
	"time"
)

// Combined is all of an environment's other logs as one: each line as the
// worker sees it written, marked with the log it is from. None of them
// says when its lines were written -- the console has no clock to speak
// of -- so the order they reach the worker is the only one there is.
const Combined = "combined"

// CombinedSize is how much of each environment's combined log is kept.
const CombinedSize = 256 << 10

// CombineEvery is how often the combined logs are read on, which is how
// finely lines of different logs are put in order.
const CombineEvery = 250 * time.Millisecond

// partialAfter is how long a line with no end yet waits for one before it
// is shown as it is: a prompt, or a program that writes a line in pieces.
const partialAfter = time.Second

// sources are the logs a combined log is made of, in the order each pass
// reads them.
var sources = []string{Worker, Console, Monitor, GPU}

// width is the widest source's name, which every line's mark is padded to.
var width = len(slices.MaxFunc(sources, func(a, b string) int { return len(a) - len(b) }))

// Combiner keeps each environment's combined log, reading on in the logs
// it is made of as they grow. It follows the environments rings has lines
// of, which is every one the worker has been asked to run.
type Combiner struct {
	rings *Rings
	files func(environment, name string) (string, bool)

	mu   sync.Mutex
	envs map[string]*combined
}

// combined is one environment's combined log and how far each of its
// sources has been read.
type combined struct {
	ring    ring
	sources map[string]*source
}

type source struct {
	next    int64
	partial []byte
	since   time.Time
	// file is the file a file's log was last read from: one put in its
	// place is another log, read from its start.
	file os.FileInfo
}

// NewCombiner combines an environment's lines of the worker's log in rings
// with the files its machine writes, which files says where they are.
func NewCombiner(rings *Rings, files func(environment, name string) (string, bool)) *Combiner {
	return &Combiner{rings: rings, files: files, envs: map[string]*combined{}}
}

// Run reads on every CombineEvery until ctx ends.
func (c *Combiner) Run(ctx context.Context) {
	t := time.NewTicker(CombineEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			c.Poll()
		}
	}
}

// Poll reads on once in every environment's logs. An environment seen for
// the first time takes its worker lines from the oldest kept, and its
// files from their ends: a machine does not outlive the worker that
// started it, so what they hold already is from before this one.
func (c *Combiner) Poll() {
	for _, env := range c.rings.environments() {
		c.mu.Lock()
		e, ok := c.envs[env]
		if !ok {
			e = &combined{ring: ring{cap: CombinedSize}, sources: map[string]*source{}}
			c.envs[env] = e
		}
		c.mu.Unlock()
		if !ok {
			for _, name := range sources {
				s := &source{}
				if path, ok := c.files(env, name); ok && name != Worker {
					if st, err := os.Stat(path); err == nil {
						s.next, s.file = st.Size(), st
					}
				}
				e.sources[name] = s
			}
		}
		for _, name := range sources {
			c.follow(env, e, name)
		}
	}
}

// read reads on in one source of an environment's combined log.
func (c *Combiner) read(env string, s *source, name string) (Reply, bool) {
	req := Request{Environment: env, Log: name, Offset: s.next}
	if name == Worker {
		r := c.rings.Read(req)
		return r, !r.Missing
	}
	path, ok := c.files(env, name)
	if !ok {
		return Reply{}, false
	}
	st, err := os.Stat(path)
	if err != nil {
		return Reply{}, false
	}
	if s.file != nil && !os.SameFile(s.file, st) {
		req.Offset = 0
	}
	s.file = st
	r, err := ReadFile(path, req)
	return r, err == nil && !r.Missing
}

// follow reads on in one source of an environment's combined log.
func (c *Combiner) follow(env string, e *combined, name string) {
	s := e.sources[name]
	r, ok := c.read(env, s, name)
	if !ok {
		return
	}
	var out bytes.Buffer
	if r.Start != s.next {
		// The log started over -- the machine booted again -- or lost its
		// oldest before it was read: what was waiting for an end has none.
		s.flush(&out, name)
	}
	s.next = r.Start + int64(len(r.Data))
	data := r.Data
	if len(s.partial) > 0 && len(data) > 0 {
		data = append(s.partial, data...)
		s.partial = nil
	}
	for {
		i := bytes.IndexByte(data, '\n')
		if i < 0 {
			break
		}
		mark(&out, name, data[:i])
		data = data[i+1:]
	}
	if len(data) > 0 {
		s.partial = bytes.Clone(data)
		s.since = time.Now()
	} else if len(s.partial) > 0 && time.Since(s.since) >= partialAfter {
		s.flush(&out, name)
	}
	if out.Len() > 0 {
		c.mu.Lock()
		e.ring.write(out.Bytes())
		c.mu.Unlock()
	}
}

func (s *source) flush(out *bytes.Buffer, name string) {
	if len(s.partial) > 0 {
		mark(out, name, s.partial)
		s.partial = nil
	}
}

// mark writes one line of a source, marked with its name.
func mark(out *bytes.Buffer, name string, line []byte) {
	fmt.Fprintf(out, "%-*s │ %s\n", width, name, bytes.TrimSuffix(line, []byte("\r")))
}

// Read reads an environment's combined log.
func (c *Combiner) Read(req Request) Reply {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.envs[req.Environment]
	if !ok {
		return Reply{Missing: true}
	}
	return e.ring.read(req)
}
