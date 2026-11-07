package clientapi

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/users"
)

// streamPing is how often an idle stream sends a comment, so proxies and
// clients can tell it is alive.
const streamPing = 30 * time.Second

// streamGather is how long a change waits for others close behind it, to
// be sent as one.
const streamGather = 50 * time.Millisecond

func (h *handler) StreamEvents(ctx context.Context, _ StreamEventsRequestObject) (StreamEventsResponseObject, error) {
	p := principal(ctx)
	// Watching before the first listing, so nothing between is missed.
	watch := h.changes.Watch()
	r, w := io.Pipe()
	go func() {
		defer watch.Close()
		w.CloseWithError(h.stream(ctx, p, watch, w))
	}()
	return StreamEvents200TextEventStreamResponse{Body: r}, nil
}

// stream writes p's environments' changes to w until ctx ends.
func (h *handler) stream(ctx context.Context, p users.Principal, watch *environments.Watch, w io.Writer) error {
	sent := map[string]bool{}
	ping := time.NewTicker(streamPing)
	defer ping.Stop()
	ids, all, synced := []string(nil), true, false
	for {
		if all {
			list, err := h.envs.List(ctx, p)
			if err != nil {
				return err
			}
			now := map[string]bool{}
			for _, e := range list {
				if e.Desired == api.DesiredDeleted {
					continue
				}
				if err := send(w, "environment", environment(e)); err != nil {
					return err
				}
				now[e.ID] = true
			}
			for id := range sent {
				if !now[id] {
					if err := send(w, "removed", map[string]string{"id": id}); err != nil {
						return err
					}
				}
			}
			sent = now
			if !synced {
				if err := send(w, "synced", struct{}{}); err != nil {
					return err
				}
				synced = true
			}
		} else {
			for _, id := range ids {
				e, err := h.envs.Get(ctx, p, id)
				switch {
				case errors.Is(err, environments.ErrNotFound) || (err == nil && e.Desired == api.DesiredDeleted):
					if sent[id] {
						delete(sent, id)
						if err := send(w, "removed", map[string]string{"id": id}); err != nil {
							return err
						}
					}
					continue
				case err != nil:
					return err
				}
				sent[id] = true
				if err := send(w, "environment", environment(e)); err != nil {
					return err
				}
			}
		}

		ids, all = nil, false
		for ids == nil && !all {
			select {
			case <-ctx.Done():
				return nil
			case <-ping.C:
				if _, err := io.WriteString(w, ": ping\n\n"); err != nil {
					return err
				}
			case <-watch.Ready():
				select {
				case <-ctx.Done():
					return nil
				case <-time.After(streamGather):
				}
				ids, all = watch.Take()
			}
		}
	}
}

// send writes one server-sent event.
func send(w io.Writer, event string, data any) error {
	b, err := json.Marshal(data)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, b)
	return err
}
