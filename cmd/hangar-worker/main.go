// Command hangar-worker runs the environments the control plane places on
// this machine. It dials out to hangar-server and needs no inbound route.
package main

import (
	"context"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/logs"
	"github.com/csnewman/hangar/internal/vm"
	"github.com/csnewman/hangar/internal/worker"
)

func main() {
	raiseFileLimit()
	config := flag.String("config", "/etc/hangar/worker.yaml", "worker configuration file")
	debug := flag.Bool("debug", false, "log at debug level")
	flag.Parse()

	level := slog.LevelInfo
	if *debug {
		level = slog.LevelDebug
	}
	// What the worker logs is kept in memory too, for the control plane to
	// show: whole, and each environment's lines.
	rings := logs.NewRings()
	log := slog.New(logs.Tee(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: level}), rings))
	slog.SetDefault(log)

	if err := run(*config, log, rings); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-worker: %v\n", err)
		os.Exit(1)
	}
}

// shutdowner is a runtime with machines to stop before the worker exits.
type shutdowner interface {
	Shutdown(ctx context.Context) error
}

func run(path string, log *slog.Logger, rings *logs.Rings) error {
	cfg, err := worker.LoadConfig(path)
	if err != nil {
		return err
	}

	var rt worker.Runtime
	switch cfg.Runtime {
	case "simulated":
		rt = worker.NewSimulated(2 * time.Second)
	case "cloud-hypervisor":
		images := map[string]vm.Image{}
		for ref, img := range cfg.VM.Images {
			images[ref] = vm.Image{Base: img.Base}
		}
		registries := map[string]vm.RegistryAuth{}
		for host, a := range cfg.VM.Registries {
			registries[host] = vm.RegistryAuth{Username: a.Username, PasswordFile: a.PasswordFile}
		}
		rt, err = vm.New(vm.Config{
			StateDir:        cfg.Storage.Environments,
			ImagesDir:       cfg.Storage.Images,
			Kernel:          cfg.VM.Kernel,
			Agent:           cfg.VM.Agent,
			Editor:          cfg.VM.Editor,
			Images:          images,
			Registries:      registries,
			UpperGiB:        cfg.VM.UpperGiB,
			DockerGiB:       cfg.VM.DockerGiB,
			ImageDevice:     cfg.VM.ImageDevice,
			Root:            cfg.VM.Root,
			DaxMiB:          cfg.VM.DaxMiB,
			GPUVenus:        cfg.VM.GPU.Venus,
			GPUVenusRestore: cfg.VM.GPU.VenusRestore,
			GPUWindowMiB:    cfg.VM.GPU.WindowMiB,
			GPURenderer:     cfg.VM.GPU.Renderer,
			GPUDevice:       cfg.VM.GPU.Device,
			Log:             log,
		})
		if err != nil {
			return err
		}
	default:
		return fmt.Errorf("unknown runtime %q: want cloud-hypervisor or simulated", cfg.Runtime)
	}

	w, err := worker.New(cfg, rt, log)
	if err == nil {
		w.UseLogs(rings)
	}
	if err != nil {
		return err
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	err = w.Run(ctx)

	if s, ok := rt.(shutdowner); ok {
		log.Info("stopping environments")
		sctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		if serr := s.Shutdown(sctx); serr != nil {
			log.Warn("environments did not all stop", "err", serr)
		}
		cancel()
	}
	if ctx.Err() != nil {
		return nil
	}
	return err
}

// raiseFileLimit raises the limit on open files to the most allowed, for the
// worker and what it starts. Go raises its own soft limit on its own, but
// gives the processes it starts the limit it was given -- often 1024 in a
// container -- unless it sets the limit itself. Cloud Hypervisor holds
// several descriptors for every queue of every device, and a large machine
// passes 1024.
func raiseFileLimit() {
	var lim syscall.Rlimit
	if err := syscall.Getrlimit(syscall.RLIMIT_NOFILE, &lim); err != nil {
		return
	}
	lim.Cur = lim.Max
	syscall.Setrlimit(syscall.RLIMIT_NOFILE, &lim)
}
