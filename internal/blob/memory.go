package blob

import (
	"bytes"
	"context"
	"io"
	"slices"
	"strings"
	"sync"
	"time"
)

// Memory is a Store in memory, for tests.
type Memory struct {
	mu      sync.Mutex
	objects map[string]memObject
}

type memObject struct {
	data     []byte
	modified time.Time
}

func NewMemory() *Memory {
	return &Memory{objects: map[string]memObject{}}
}

func (m *Memory) Put(ctx context.Context, key string, r io.Reader, size int64) error {
	data, err := io.ReadAll(r)
	if err != nil {
		return err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	m.objects[key] = memObject{data: data, modified: time.Now()}
	return nil
}

type memReader struct{ *bytes.Reader }

func (memReader) Close() error { return nil }

func (r memReader) Size() int64 { return r.Reader.Size() }

func (m *Memory) Get(ctx context.Context, key string) (Reader, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	o, ok := m.objects[key]
	if !ok {
		return nil, ErrNotFound
	}
	return memReader{bytes.NewReader(o.data)}, nil
}

func (m *Memory) Stat(ctx context.Context, key string) (Object, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	o, ok := m.objects[key]
	if !ok {
		return Object{}, ErrNotFound
	}
	return Object{Key: key, Size: int64(len(o.data)), Modified: o.modified}, nil
}

func (m *Memory) Copy(ctx context.Context, src, dst string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	o, ok := m.objects[src]
	if !ok {
		return ErrNotFound
	}
	m.objects[dst] = memObject{data: o.data, modified: time.Now()}
	return nil
}

func (m *Memory) Delete(ctx context.Context, key string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.objects, key)
	return nil
}

func (m *Memory) List(ctx context.Context, prefix string) ([]Object, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []Object
	for k, o := range m.objects {
		if strings.HasPrefix(k, prefix) {
			out = append(out, Object{Key: k, Size: int64(len(o.data)), Modified: o.modified})
		}
	}
	slices.SortFunc(out, func(a, b Object) int { return strings.Compare(a.Key, b.Key) })
	return out, nil
}
