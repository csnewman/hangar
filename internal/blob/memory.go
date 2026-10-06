package blob

import (
	"bytes"
	"context"
	"io"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Memory is a Store and a Versioned store in memory, for tests. Its
// versioned objects are apart from its others.
type Memory struct {
	mu       sync.Mutex
	objects  map[string]memObject
	versions map[string]map[string][]byte
	next     int
}

type memObject struct {
	data     []byte
	modified time.Time
}

func NewMemory() *Memory {
	return &Memory{objects: map[string]memObject{}, versions: map[string]map[string][]byte{}}
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

func (m *Memory) PutVersion(ctx context.Context, key string, r io.Reader, size int64) (string, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return "", err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	m.next++
	id := strconv.Itoa(m.next)
	if m.versions[key] == nil {
		m.versions[key] = map[string][]byte{}
	}
	m.versions[key][id] = data
	return id, nil
}

func (m *Memory) GetVersion(ctx context.Context, key, version string) (Reader, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	data, ok := m.versions[key][version]
	if !ok {
		return nil, ErrNotFound
	}
	return memReader{bytes.NewReader(data)}, nil
}

func (m *Memory) DeleteVersion(ctx context.Context, key, version string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.versions[key], version)
	if len(m.versions[key]) == 0 {
		delete(m.versions, key)
	}
	return nil
}

func (m *Memory) DeleteAll(ctx context.Context, prefix string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	for k := range m.versions {
		if strings.HasPrefix(k, prefix) {
			delete(m.versions, k)
		}
	}
	return nil
}

// Versions returns how many versions the versioned store holds of each
// key, for tests to see what is left.
func (m *Memory) Versions() map[string]int {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := map[string]int{}
	for k, vs := range m.versions {
		out[k] = len(vs)
	}
	return out
}
