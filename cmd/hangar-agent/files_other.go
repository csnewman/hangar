//go:build !linux

package main

import (
	"errors"
	"os/user"

	"github.com/csnewman/hangar/internal/profile"
)

// router is Linux's alone: the shared files are routed by hangarfs.
type router struct{}

func newRouter(*user.User) *router { return &router{} }

// sharedDir and agentState are where a Linux guest mounts the shared
// files and keeps what it routed.
const (
	sharedDir  = "/run/hangar/files"
	agentState = "/var/lib/hangar-agent"
)

func (*router) mount() error {
	return errors.New("the shared files are only mounted in a Linux guest")
}

func (*router) apply([]profile.Route) error {
	return errors.New("the shared files are only routed in a Linux guest")
}
