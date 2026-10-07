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

func (*router) apply([]profile.Route) error {
	return errors.New("the shared files are only routed in a Linux guest")
}
