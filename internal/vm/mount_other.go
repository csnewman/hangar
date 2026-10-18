//go:build !linux

package vm

import "errors"

// Only Linux has mounted a copy's image.
func unmount(string) error { return errors.New("unmounting needs Linux") }

func isEROFS(string) bool { return false }
