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

func run(path string, log *slog.Logger) error {
	cfg, err := worker.LoadConfig(path)
	if err != nil {
		return err
	}

	var rt worker.Runtime
	switch cfg.Runtime {
	case "simulated":
		rt = worker.NewSimulated(2 * time.Second)
	default:
		return fmt.Errorf("runtime %q is not available; the only runtime is \"simulated\"", cfg.Runtime)
	}

	w, err := worker.New(cfg, rt, log)
	if err != nil {
		return err
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	err = w.Run(ctx)
	if ctx.Err() != nil {
		return nil
	}
	return err
}
