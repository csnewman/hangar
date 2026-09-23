// Package api holds the JSON shapes exchanged with hangar-server: the public
// API the web UI and CLI use, and the worker API.
//
// The worker API is level-triggered. The server never sends a worker a
// command; it publishes the complete set of environments the worker should be
// holding, and the worker reports the complete set it is actually holding.
// Either side can drop any number of messages and the next one repairs it.
package api

import "time"

// DesiredState is what the owner of an environment has asked for.
type DesiredState string

const (
	DesiredRunning DesiredState = "running"
	DesiredStopped DesiredState = "stopped"
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
	PhaseFailed   Phase = "failed"
	PhaseDeleting Phase = "deleting"
)

// Environment is the public view of one environment.
type Environment struct {
	ID        string       `json:"id"`
	Name      string       `json:"name"`
	Image     string       `json:"image"`
	CPUs      int          `json:"cpus"`
	MemoryMiB int          `json:"memory_mib"`
	Desired   DesiredState `json:"desired"`
	Phase     Phase        `json:"phase"`
	Reason    string       `json:"reason,omitempty"`
	WorkerID  string       `json:"worker_id,omitempty"`
	Worker    string       `json:"worker,omitempty"`
	CreatedAt time.Time    `json:"created_at"`
	UpdatedAt time.Time    `json:"updated_at"`
}

// CreateEnvironment is the body of a request to create an environment.
type CreateEnvironment struct {
	Name      string `json:"name"`
	Image     string `json:"image"`
	CPUs      int    `json:"cpus"`
	MemoryMiB int    `json:"memory_mib"`
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
	Unknown    []string   `json:"unknown"`
	LastSeenAt *time.Time `json:"last_seen_at,omitempty"`
	CreatedAt  time.Time  `json:"created_at"`
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
}

// EnvironmentSpec is one environment as a worker needs to see it.
type EnvironmentSpec struct {
	ID        string       `json:"id"`
	Name      string       `json:"name"`
	Image     string       `json:"image"`
	CPUs      int          `json:"cpus"`
	MemoryMiB int          `json:"memory_mib"`
	Desired   DesiredState `json:"desired"`
}

// WorkerStatus is everything a worker is holding, and what it can offer.
type WorkerStatus struct {
	Capacity     Resources             `json:"capacity"`
	Labels       map[string]string     `json:"labels"`
	Environments []ObservedEnvironment `json:"environments"`
}

// ObservedEnvironment is one environment as the worker finds it.
type ObservedEnvironment struct {
	ID     string `json:"id"`
	Phase  Phase  `json:"phase"`
	Reason string `json:"reason,omitempty"`
}
