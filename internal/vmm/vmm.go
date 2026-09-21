// Package vmm runs a guest under hangar-vmm, the KVM monitor in vmm/.
//
// The monitor exists to run exactly one environment, so there is no API and
// no command line to speak of: it is handed a JSON description of the machine
// on standard input and runs until the guest stops. That description is the
// Config below, and this package is the only thing that writes it.
package vmm

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"syscall"
)

// Binary is where `hangar vmm` leaves its build.
const Binary = "vmm/target/release/hangar-vmm"

// Disk is a block device attached to the guest.
type Disk struct {
	Path     string `json:"path"`
	ReadOnly bool   `json:"readOnly,omitempty"`
}

// Fs is a host directory exported to the guest over virtio-fs.
type Fs struct {
	SharedDir string `json:"sharedDir"`
	Tag       string `json:"tag"`
	// DaxMib sizes the DAX window. Zero leaves it out, which makes every
	// read a FUSE request.
	DaxMib int `json:"daxMib"`
	Queues int `json:"queues,omitempty"`
}

// Balloon configures memory reclaim.
type Balloon struct {
	FreePageReporting bool `json:"freePageReporting"`
}

// Config is the machine, as the monitor reads it.
type Config struct {
	Name      string   `json:"name,omitempty"`
	Kernel    string   `json:"kernel"`
	Initrd    string   `json:"initrd,omitempty"`
	Cmdline   string   `json:"cmdline,omitempty"`
	MemoryMib int      `json:"memoryMib"`
	CPUs      int      `json:"cpus"`
	Disks     []Disk   `json:"disks,omitempty"`
	Fs        *Fs      `json:"fs,omitempty"`
	VsockCID  uint32   `json:"vsockCid,omitempty"`
	Balloon   *Balloon `json:"balloon,omitempty"`
	Console   string   `json:"console,omitempty"`
}

// Find locates the monitor, preferring the build in this tree.
func Find() (string, error) {
	candidates := []string{
		Binary,
		filepath.Join("vmm", "target", "debug", "hangar-vmm"),
		"/usr/local/bin/hangar-vmm",
	}
	for _, p := range candidates {
		if _, err := os.Stat(p); err == nil {
			return filepath.Abs(p)
		}
	}
	return "", fmt.Errorf("hangar-vmm not found in %v: build it with \"hangar vmm\"", candidates)
}

// Run starts the monitor and blocks until the guest stops.
//
// Stdio is the guest's console when Config.Console is empty, so the caller's
// terminal becomes the guest's.
func Run(ctx context.Context, cfg *Config) error {
	bin, err := Find()
	if err != nil {
		return err
	}
	body, err := json.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("encoding the machine description: %w", err)
	}

	// Ctrl-C should stop the guest rather than kill us and orphan it.
	ctx, stop := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer stop()

	cmd := exec.CommandContext(ctx, bin)
	cmd.Stdin = bytes.NewReader(body)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }

	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s: %w", filepath.Base(bin), err)
	}
	return nil
}
