package vm

import (
	"time"
)

// lookUpImagesEvery is how often the references environments here are
// pinned to are looked up, so a reference that has moved on -- a tag pushed
// again, a local source rebuilt -- is offered to them as an update.
const lookUpImagesEvery = 2 * time.Minute

// watchImages looks up, at the start and then every lookUpImagesEvery, what
// each image reference an environment here is pinned to names, and tells
// the server when one has moved on. A copy left superseded and unused is
// removed.
func (r *Runtime) watchImages() {
	t := time.NewTicker(lookUpImagesEvery)
	defer t.Stop()
	for {
		r.lookUpImages()
		select {
		case <-r.ctx.Done():
			return
		case <-t.C:
		}
	}
}

func (r *Runtime) lookUpImages() {
	refs := map[string]bool{}
	r.mu.Lock()
	for _, m := range r.envs {
		m.mu.Lock()
		if m.image != nil {
			refs[m.image.Ref] = true
		}
		m.mu.Unlock()
	}
	r.mu.Unlock()
	changed := false
	for ref := range refs {
		before, known := r.store.Latest(ref)
		d, err := r.store.Current(r.ctx, ref)
		if err != nil {
			r.cfg.Log.Debug("looking up an image", "image", ref, "err", err)
			continue
		}
		if !known || d != before {
			changed = true
		}
	}
	if changed {
		r.prune()
		r.notify()
	}
}
