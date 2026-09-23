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

	"github.com/csnewman/hangar/internal/vm"
	"github.com/csnewman/hangar/internal/worker"
)

func main() {
	config := flag.String("config", "/etc/hangar/worker.yaml", "worker configuration file")
	debug := flag.Bool("debug", false, "log at debug level")
	flag.Parse()

	level := slog.LevelInfo
	if *debug {
		level = slog.LevelDebug
	}
	log := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: level}))
	slog.SetDefault(log)

	if err := run(*config, log); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-worker: %v\n", err)
		os.Exit(1)
	}
}

// shutdowner is a runtime with machines to stop before the worker exits.
type shutdowner interface {
	Shutdown(ctx context.Context) error
}

func run(path string, log *slog.Logger) error {
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
			images[ref] = vm.Image{Base: img.Base, Initrd: img.Initrd}
		}
		rt, err = vm.New(vm.Config{
			StateDir:  cfg.Storage.Environments,
			Kernel:    cfg.VM.Kernel,
			Images:    images,
			UpperGiB:  cfg.VM.UpperGiB,
			DockerGiB: cfg.VM.DockerGiB,
			DaxMiB:    cfg.VM.DaxMiB,
			Log:       log,
		})
		if err != nil {
			return err
		}
	default:
		return fmt.Errorf("unknown runtime %q: want cloud-hypervisor or simulated", cfg.Runtime)
	}

	w, err := worker.New(cfg, rt, log)
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
