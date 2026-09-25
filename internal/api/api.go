// Package api holds the JSON shapes exchanged with hangar-server: the public
// API the web UI and CLI use, and the worker API.
//
// The worker API is level-triggered. The server never sends a worker a
// command; it publishes the complete set of environments the worker should be
// holding, and the worker reports the complete set it is actually holding.
// Either side can drop any number of messages and the next one repairs it.
package api

import (
	"slices"
	"time"
)

// DesiredState is what the owner of an environment has asked for.
type DesiredState string

const (
	DesiredRunning DesiredState = "running"
	DesiredStopped DesiredState = "stopped"
	// DesiredSuspended keeps a running environment's memory on its worker's
	// disk, and its processes with it, until it is started again. It stops
	// using memory and CPU as a stopped one does, and resumes where it was
	// rather than booting. Stopping a suspended environment discards the
	// memory; its disks stay as they were, as after a power cut.
	DesiredSuspended DesiredState = "suspended"
	// DesiredDeleted is held until the worker reports the environment gone,
	// and the row is removed then. An environment absent from a desired set is
	// one the server does not know, which a worker must leave alone.
	DesiredDeleted DesiredState = "deleted"
)

// Phase is what a worker reports an environment to be doing.
type Phase string

const (
	// PhasePending is the server's own phase for an environment no worker
	// has reported on yet, whether or not it has been placed.
	PhasePending  Phase = "pending"
	PhaseStarting Phase = "starting"
	PhaseRunning  Phase = "running"
	PhaseStopping Phase = "stopping"
	PhaseStopped  Phase = "stopped"
	// PhaseSuspending and PhaseSuspended are an environment being written to
	// disk and held there. A start resumes a suspended environment; its
	// phase is starting while it does.
	PhaseSuspending Phase = "suspending"
	PhaseSuspended  Phase = "suspended"
	PhaseFailed     Phase = "failed"
	PhaseDeleting   Phase = "deleting"
)

// Display is whether an environment has a graphical desktop.
type Display string

const (
	// DisplayNone is a headless environment: no compositor, no VNC, and
	// less memory spent on both.
	DisplayNone    Display = "none"
	DisplayDesktop Display = "desktop"
)

// GPU is what kind of GPU an environment is given.
type GPU string

const (
	GPUNone GPU = "none"
	// GPUVirtual renders through the host's GPU via virtio-gpu, shared with
	// other environments.
	GPUVirtual GPU = "virtual"
	// GPUPassthrough hands the environment a whole physical GPU. It pins the
	// environment's memory and cannot be suspended.
	GPUPassthrough GPU = "passthrough"
)

// Repo is a git repository cloned into an environment when it is created.
type Repo struct {
	URL string `json:"url"`
	// Ref is what to check out: a branch, tag or commit. Empty means the
	// remote's default branch.
	Ref string `json:"ref,omitempty"`
	// Path is where in the environment the repository is cloned.
	Path string `json:"path"`
	// Branch, if set, is a new branch created from Ref after cloning. In a
	// template it may contain {name}, replaced by the environment's name; in
	// an environment it is the resolved name.
	Branch string `json:"branch,omitempty"`
}

// Spec is what an environment is. A template holds one, with patterns in it;
// an environment holds its own resolved copy, made when it was created.
type Spec struct {
	Image     string  `json:"image"`
	CPUs      int     `json:"cpus"`
	MemoryMiB int     `json:"memory_mib"`
	Display   Display `json:"display"`
	GPU       GPU     `json:"gpu"`
	Repos     []Repo  `json:"repos"`
	// EditorPath is the folder the editor opens on.
	EditorPath string `json:"editor_path,omitempty"`
	// Untrusted is for code the owner does not trust: the environment is
	// never given their credentials (internal/profile), and its editor
	// trusts no folder.
	Untrusted bool `json:"untrusted,omitempty"`
	// TrustedFolders are trusted by the editor without asking, beyond the
	// repositories and the editor's folder, which a trusted environment's
	// editor trusts anyway. Ignored for an untrusted one.
	TrustedFolders []string `json:"trusted_folders,omitempty"`
	// Placement limits which workers may run the environment: each key must
	// be a label the worker has, with this value.
	Placement map[string]string `json:"placement"`
}

// EditorTrust is the folders an environment's editor trusts without asking:
// none for one that is untrusted; otherwise its repositories, its editor's
// folder, and whatever else its template names.
func (s Spec) EditorTrust() []string {
	out := []string{}
	if s.Untrusted {
		return out
	}
	add := func(p string) {
		if p != "" && !slices.Contains(out, p) {
			out = append(out, p)
		}
	}
	for _, r := range s.Repos {
		add(r.Path)
	}
	add(s.EditorPath)
	for _, f := range s.TrustedFolders {
		add(f)
	}
	return out
}

// TemplateSpec is a Spec with the rules for naming the environments made
// from it.
type TemplateSpec struct {
	Spec
	// NamePattern, if set, is a regular expression an environment's whole
	// name must match -- a ticket ID, say, which then names its branch.
	NamePattern string `json:"name_pattern,omitempty"`
	// NameHint tells a person what name the pattern wants.
	NameHint string `json:"name_hint,omitempty"`
}

// Environment is the public view of one environment.
type Environment struct {
	ID      string `json:"id"`
	OwnerID string `json:"owner_id"`
	Owner   string `json:"owner"`
	Name    string `json:"name"`
	// TemplateID is empty once the template has been deleted; Template
	// keeps its name.
	TemplateID string            `json:"template_id,omitempty"`
	Template   string            `json:"template"`
	Spec       Spec              `json:"spec"`
	Image      string            `json:"image"`
	CPUs       int               `json:"cpus"`
	MemoryMiB  int               `json:"memory_mib"`
	Desired    DesiredState      `json:"desired"`
	Phase      Phase             `json:"phase"`
	Reason     string            `json:"reason,omitempty"`
	Progress   *Progress         `json:"progress,omitempty"`
	WorkerID   string            `json:"worker_id,omitempty"`
	Worker     string            `json:"worker,omitempty"`
	Stats      *EnvironmentStats `json:"stats,omitempty"`
	CreatedAt  time.Time         `json:"created_at"`
	UpdatedAt  time.Time         `json:"updated_at"`
}

// CreateEnvironment is a request to create an environment from a template.
type CreateEnvironment struct {
	TemplateID string `json:"template_id"`
	Name       string `json:"name"`
}

// Worker is the public view of one worker.
type Worker struct {
	ID       string            `json:"id"`
	Name     string            `json:"name"`
	Labels   map[string]string `json:"labels"`
	Capacity Resources         `json:"capacity"`
	// Allocated is the sum of what the environments placed on the worker ask
	// for, whether or not they are running.
	Allocated Resources `json:"allocated"`
	Online    bool      `json:"online"`
	Revoked   bool      `json:"revoked"`
	// Unknown lists environments the worker is running that the server has no
	// record of. They are reported and never stopped automatically: the likely
	// cause is a control plane that lost data, not a rogue VM.
	Unknown    []string     `json:"unknown"`
	LastSeenAt *time.Time   `json:"last_seen_at,omitempty"`
	CreatedAt  time.Time    `json:"created_at"`
	Stats      *WorkerStats `json:"stats,omitempty"`
	Images     []LocalImage `json:"images"`
	// PendingRemovals are images the worker has been asked to delete and
	// still holds.
	PendingRemovals []string `json:"pending_removals"`
}

// Resources is an amount of CPU and memory.
type Resources struct {
	CPUs      int `json:"cpus"`
	MemoryMiB int `json:"memory_mib"`
}

// Error is the body of every non-2xx response.
type Error struct {
	Error string `json:"error"`
}

// RegisterWorker is sent once, with the bootstrap token, to exchange it for a
// credential of the worker's own.
type RegisterWorker struct {
	Name   string            `json:"name"`
	Labels map[string]string `json:"labels"`
}

// WorkerCredential is the reply to RegisterWorker. The credential is shown
// once; the server keeps only its hash.
type WorkerCredential struct {
	ID         string `json:"id"`
	Credential string `json:"credential"`
}

// DesiredSet is everything the server wants a worker to hold. Version
// increases whenever the set changes, and a worker passes back the last one
// it saw so the server can hold the request until there is something new.
type DesiredSet struct {
	Version      int64             `json:"version"`
	Environments []EnvironmentSpec `json:"environments"`
	// RemoveImages are images the worker should delete from its store. One
	// an environment still uses is kept, and stays asked for.
	RemoveImages []string `json:"remove_images"`
	// RemoveEnvironments are environments the worker runs that the server
	// has no record of, which it should power off and delete, disks and
	// all.
	RemoveEnvironments []string `json:"remove_environments"`
}

// EnvironmentSpec is one environment as a worker needs to see it.
type EnvironmentSpec struct {
	ID      string       `json:"id"`
	Name    string       `json:"name"`
	Desired DesiredState `json:"desired"`
	Spec    Spec         `json:"spec"`
}

// WorkerStatus is everything a worker is holding, and what it can offer.
type WorkerStatus struct {
	Capacity     Resources             `json:"capacity"`
	Labels       map[string]string     `json:"labels"`
	Environments []ObservedEnvironment `json:"environments"`
	Stats        *WorkerStats          `json:"stats,omitempty"`
	Images       []LocalImage          `json:"images"`
}

// ObservedEnvironment is one environment as the worker finds it.
type ObservedEnvironment struct {
	ID     string `json:"id"`
	Phase  Phase  `json:"phase"`
	Reason string `json:"reason,omitempty"`
	// Progress is how far a starting environment has got.
	Progress *Progress         `json:"progress,omitempty"`
	Stats    *EnvironmentStats `json:"stats,omitempty"`
}

// Step is one step of starting an environment. They happen in the order
// declared; one with nothing to do -- an image the worker already holds, a
// workspace set up on an earlier boot -- is passed over.
type Step string

const (
	// StepDownload fetches the image from its registry, or copies it from
	// the worker's own source for it.
	StepDownload Step = "download"
	// StepUnpack applies the image's layers into its root filesystem.
	StepUnpack Step = "unpack"
	// StepDisks makes the environment's disks, the first time.
	StepDisks Step = "disks"
	// StepBoot boots or resumes the machine, until its agent answers.
	StepBoot Step = "boot"
	// StepServices waits for the guest's own services to start.
	StepServices Step = "services"
	// StepWorkspace applies the template: the hostname and the clones.
	StepWorkspace Step = "workspace"
)

// Progress is the step a starting environment is on and, where the step
// can be measured, how much of it is done: Done of Total in Unit, "bytes"
// or "objects", going at Rate a second lately. A zero Total is a step that
// cannot be measured.
type Progress struct {
	Step  Step    `json:"step"`
	Done  int64   `json:"done,omitempty"`
	Total int64   `json:"total,omitempty"`
	Unit  string  `json:"unit,omitempty"`
	Rate  float64 `json:"rate,omitempty"`
}

// EnvironmentStats is what an environment is using, measured by its worker.
// Rates are per second, averaged since the previous measurement.
type EnvironmentStats struct {
	// CPUPercent is of the environment's own vCPUs: 100 is all of them busy.
	CPUPercent     float64 `json:"cpu_percent"`
	MemoryUsedMiB  int     `json:"memory_used_mib"`
	MemoryTotalMiB int     `json:"memory_total_mib"`
	// DiskUsedBytes is what its writable layer and Docker store take up on
	// the worker.
	DiskUsedBytes int64   `json:"disk_used_bytes"`
	DiskReadBps   float64 `json:"disk_read_bps"`
	DiskWriteBps  float64 `json:"disk_write_bps"`
	NetRxBps      float64 `json:"net_rx_bps"`
	NetTxBps      float64 `json:"net_tx_bps"`
}

// WorkerStats is the load on a worker's machine as a whole.
type WorkerStats struct {
	CPUPercent     float64 `json:"cpu_percent"`
	Load1          float64 `json:"load1"`
	MemoryUsedMiB  int     `json:"memory_used_mib"`
	MemoryTotalMiB int     `json:"memory_total_mib"`
	// DiskUsedBytes and DiskTotalBytes are for the filesystem environments
	// are kept on.
	DiskUsedBytes  int64 `json:"disk_used_bytes"`
	DiskTotalBytes int64 `json:"disk_total_bytes"`
}

// LocalImage is an image a worker holds in its own store.
type LocalImage struct {
	Ref       string `json:"ref"`
	SizeBytes int64  `json:"size_bytes"`
	// State is "ready", or "fetching" while it is being pulled or copied
	// in.
	State string `json:"state"`
	// Environments lists the environments on the worker using it.
	Environments []string `json:"environments"`
}
