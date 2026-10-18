//go:build !linux

package erofs

// readXattrs reads no attributes: images are built from Linux trees, whose
// attributes only Linux reads as they are.
func readXattrs(string) (map[string][]byte, error) { return nil, nil }
