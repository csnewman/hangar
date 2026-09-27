package environments

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"slices"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// The settings an environment may differ from its template in, as
// TemplateChanges names them.
const (
	SettingImage          = "image"
	SettingCPUs           = "cpus"
	SettingMemory         = "memory"
	SettingDisplay        = "display"
	SettingGPU            = "gpu"
	SettingDAX            = "dax"
	SettingRepos          = "repos"
	SettingEditorPath     = "editor_path"
	SettingUntrusted      = "untrusted"
	SettingTrustedFolders = "trusted_folders"
	SettingPlacement      = "placement"
	// SettingName is that the template does not accept the environment's
	// name, whose pattern it has changed.
	SettingName = "name"
)

// templateChanges are the settings in which an environment's spec differs
// from what its template's current settings would give an environment of
// its name.
func templateChanges(env api.Spec, t api.TemplateSpec, name string) []string {
	want, err := templates.Resolve(t, name)
	if err != nil {
		return []string{SettingName}
	}
	var out []string
	add := func(differ bool, s string) {
		if differ {
			out = append(out, s)
		}
	}
	add(env.Image != want.Image, SettingImage)
	add(env.CPUs != want.CPUs, SettingCPUs)
	add(env.MemoryMiB != want.MemoryMiB, SettingMemory)
	add(env.Display != want.Display, SettingDisplay)
	add(env.GPU != want.GPU, SettingGPU)
	add(env.DAX != want.DAX, SettingDAX)
	add(!slices.Equal(env.Repos, want.Repos), SettingRepos)
	add(env.EditorPath != want.EditorPath, SettingEditorPath)
	add(env.Untrusted != want.Untrusted, SettingUntrusted)
	add(!slices.Equal(env.TrustedFolders, want.TrustedFolders), SettingTrustedFolders)
	add(!maps.Equal(env.Placement, want.Placement), SettingPlacement)
	return out
}

// Settings are what of an environment may change after it is made: its
// size, whether it has a desktop and a virtual GPU, and whether its image's
// files are mapped from the host. They are the machine's, so they take
// effect when it next starts.
type Settings struct {
	CPUs      int
	MemoryMiB int
	Display   api.Display
	GPU       api.GPU
	DAX       bool
}

// changing reads and locks an environment p may reach for a change to its
// settings, which it must be stopped for: a running machine has its size,
// and a suspended one's saved memory was made at it.
func changing(ctx context.Context, tx db.Tx, p users.Principal, id string) (api.Environment, error) {
	if !db.ValidUUID(id) {
		return api.Environment{}, ErrNotFound
	}
	if _, err := tx.Exec(ctx, `SELECT 1 FROM environments e WHERE `+visible+` AND e.id = $3 FOR UPDATE`,
		p.Admin, p.UserID, id); err != nil {
		return api.Environment{}, err
	}
	e, err := scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+` AND e.id = $3`,
		p.Admin, p.UserID, id))
	if err != nil {
		return e, err
	}
	switch {
	case e.Desired == api.DesiredSuspended || e.Phase == api.PhaseSuspended:
		return e, fmt.Errorf("%w: the environment is suspended; stop it first, which ends what is running in it", ErrConflict)
	// One not yet placed has no machine to start with the settings it had.
	case e.Desired != api.DesiredStopped && !(e.WorkerID == "" && e.Phase == api.PhasePending) ||
		e.Phase == api.PhaseStopping:
		return e, fmt.Errorf("%w: stop the environment first; the change takes effect when it next starts", ErrConflict)
	}
	return e, nil
}

// saveSpec stores an environment's changed spec, and the template revision
// it follows when it was reset to its template.
func saveSpec(ctx context.Context, tx db.Tx, id string, spec api.Spec, revision *int64) error {
	raw, err := json.Marshal(spec)
	if err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `UPDATE environments SET spec = $2, image = $3, cpus = $4, memory_mib = $5,
		template_revision = coalesce($6, template_revision), updated_at = now() WHERE id = $1`,
		id, raw, spec.Image, spec.CPUs, spec.MemoryMiB, revision)
	return err
}

// checkGPU fails for a virtual GPU on a worker that offers none: one
// without a GPU backend, or whose backend cannot render as asked.
func checkGPU(ctx context.Context, tx db.Tx, e api.Environment, gpu api.GPU) error {
	if gpu != api.GPUVirtual || e.WorkerID == "" {
		return nil
	}
	var raw []byte
	if err := tx.QueryRow(ctx, `SELECT gpu FROM workers WHERE id = $1`, e.WorkerID).Scan(&raw); err != nil {
		return err
	}
	if raw == nil {
		return fmt.Errorf("%w: its worker, %s, has no virtual GPU to offer", ErrInvalid, e.Worker)
	}
	var g api.GPUInfo
	if err := json.Unmarshal(raw, &g); err != nil {
		return err
	}
	if g.Unavailable != "" {
		return fmt.Errorf("%w: its worker, %s, offers no virtual GPU: %s", ErrInvalid, e.Worker, g.Unavailable)
	}
	return nil
}

// UpdateSettings changes a stopped environment's size, display, GPU and
// whether its image's files are mapped from the host.
// They are held to what a template may ask, and a GPU passed through is
// only a template's to ask for.
func (m *Manager) UpdateSettings(ctx context.Context, p users.Principal, id string, s Settings) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := changing(ctx, tx, p, id)
		if err != nil {
			return err
		}
		spec := cur.Spec
		spec.CPUs, spec.MemoryMiB, spec.Display, spec.GPU, spec.DAX = s.CPUs, s.MemoryMiB, s.Display, s.GPU, s.DAX
		if s.GPU == api.GPUPassthrough && cur.Spec.GPU != api.GPUPassthrough {
			return fmt.Errorf("%w: a GPU passed through is only a template's to ask for", ErrInvalid)
		}
		checked, err := templates.Validate(api.TemplateSpec{Spec: spec})
		if err != nil {
			return fmt.Errorf("%w%s", ErrInvalid, trimInvalid(err))
		}
		spec = checked.Spec
		if err := checkGPU(ctx, tx, cur, spec.GPU); err != nil {
			return err
		}
		if err := saveSpec(ctx, tx, id, spec, nil); err != nil {
			return err
		}
		if e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id)); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "environment.update_settings", Target: Ref(id, cur.Name),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: cur.OwnerID}},
			Details: map[string]any{
				"cpus":       map[string]int{"from": cur.Spec.CPUs, "to": spec.CPUs},
				"memory_mib": map[string]int{"from": cur.Spec.MemoryMiB, "to": spec.MemoryMiB},
				"display":    map[string]api.Display{"from": cur.Spec.Display, "to": spec.Display},
				"gpu":        map[string]api.GPU{"from": cur.Spec.GPU, "to": spec.GPU},
				"dax":        map[string]bool{"from": cur.Spec.DAX, "to": spec.DAX},
			}})
	})
	return e, err
}

// ResetToTemplate gives a stopped environment its template's current
// settings, keeping its disks and everything on them. What cannot
// change under those disks refuses it: another image, which its writable
// layer was not made over; other repositories, which were cloned when it
// was made; and placement its worker, where its disks are, does not meet.
func (m *Manager) ResetToTemplate(ctx context.Context, p users.Principal, id string) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := changing(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if cur.TemplateID == "" {
			return fmt.Errorf("%w: its template, %s, has been deleted", ErrConflict, cur.Template)
		}
		_, tspec, revision, err := templates.ForUse(ctx, tx, p, cur.TemplateID)
		if errors.Is(err, templates.ErrNotFound) {
			return fmt.Errorf("%w: its template, %s, is not one you may use", ErrConflict, cur.Template)
		}
		if err != nil {
			return err
		}
		spec, err := templates.Resolve(tspec, cur.Name)
		if err != nil {
			return fmt.Errorf("%w: its template does not accept the name %s%s", ErrConflict, cur.Name, trimInvalid(err))
		}
		if spec.Image != cur.Spec.Image {
			return fmt.Errorf("%w: the template names another image, %s; make a new environment from it", ErrConflict, spec.Image)
		}
		if !slices.Equal(spec.Repos, cur.Spec.Repos) {
			return fmt.Errorf("%w: the template has other repositories, which are cloned only when an environment is made; make a new environment from it", ErrConflict)
		}
		if cur.WorkerID != "" && len(spec.Placement) > 0 {
			var labels []byte
			if err := tx.QueryRow(ctx, `SELECT labels FROM workers WHERE id = $1`, cur.WorkerID).Scan(&labels); err != nil {
				return err
			}
			have := map[string]string{}
			if err := json.Unmarshal(labels, &have); err != nil {
				return err
			}
			for k, v := range spec.Placement {
				if have[k] != v {
					return fmt.Errorf("%w: the template's placement wants %s=%s, which its worker, %s, where its disks are, does not have",
						ErrConflict, k, v, cur.Worker)
				}
			}
		}
		if err := checkGPU(ctx, tx, cur, spec.GPU); err != nil {
			return err
		}
		if err := saveSpec(ctx, tx, id, spec, &revision); err != nil {
			return err
		}
		if e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id)); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "environment.reset_to_template", Target: Ref(id, cur.Name),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: cur.OwnerID},
				{Type: audit.KindTemplate, ID: cur.TemplateID, Name: cur.Template}},
			Details: map[string]any{"changed": cur.TemplateChanges, "revision": revision}})
	})
	if errors.Is(err, pgx.ErrNoRows) {
		err = ErrNotFound
	}
	return e, err
}

// trimInvalid is a templates error's text after its sentinel, to go after
// one of this package's.
func trimInvalid(err error) string {
	s := err.Error()
	if t := templates.ErrInvalid.Error(); len(s) > len(t) && s[:len(t)] == t {
		return s[len(t):]
	}
	return ": " + s
}
