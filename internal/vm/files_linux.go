//go:build linux

package vm

import (
	"context"
	"errors"
	"net"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/nfs"
	"github.com/csnewman/hangar/internal/vsock"
)

// sharedFiles serves a guest its shared files over NFS, on the vsock port
// its agent mounts them from.
type sharedFiles struct {
	ln     *vsock.HybridListener
	srv    *nfs.Server
	cancel context.CancelFunc

	mu     sync.Mutex
	closed bool
	conns  map[net.Conn]bool
}

func serveSharedFiles(ctx context.Context, base string, cid uint32, cfg InstanceConfig) (*sharedFiles, error) {
	ctx, cancel := context.WithCancel(ctx)
	srv, err := nfs.New(nfs.Config{Root: cfg.SharedFiles, View: cfg.SharedView(ctx), Log: cfg.Log.With("component", "nfs")})
	if err != nil {
		cancel()
		return nil, err
	}
	ln, err := vsock.ListenHybrid(base, sharedFilesPort, cid)
	if err != nil {
		cancel()
		srv.Close()
		return nil, err
	}
	f := &sharedFiles{ln: ln, srv: srv, cancel: cancel, conns: map[net.Conn]bool{}}
	go f.accept(cfg)
	return f, nil
}

func (f *sharedFiles) accept(cfg InstanceConfig) {
	for {
		file, _, err := f.ln.Accept(time.Second)
		f.mu.Lock()
		closed := f.closed
		f.mu.Unlock()
		if closed {
			if file != nil {
				file.Close()
			}
			return
		}
		if errors.Is(err, vsock.ErrAcceptTimeout) {
			continue
		}
		if err != nil {
			cfg.Log.Warn("accepting the guest's shared files connection", "err", err)
			time.Sleep(time.Second)
			continue
		}
		conn, err := net.FileConn(file)
		file.Close()
		if err != nil {
			continue
		}
		f.mu.Lock()
		f.conns[conn] = true
		f.mu.Unlock()
		go func() {
			if err := f.srv.Serve(conn); err != nil {
				cfg.Log.Debug("the shared files connection ended", "err", err)
			}
			f.mu.Lock()
			delete(f.conns, conn)
			f.mu.Unlock()
		}()
	}
}

// Close stops serving: connections end, and the guest's state -- its open
// files and locks -- goes with them.
func (f *sharedFiles) Close() error {
	f.mu.Lock()
	f.closed = true
	for c := range f.conns {
		c.Close()
	}
	f.mu.Unlock()
	f.ln.Close()
	f.srv.Close()
	f.cancel()
	return nil
}
