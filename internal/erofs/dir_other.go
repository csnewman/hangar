//go:build !unix

package erofs

import "errors"

// AddDir reads owners and modes as Unix keeps them, which this system does
// not.
func (w *Writer) AddDir(string) error {
	return errors.New("erofs: adding a directory needs a Unix system")
}
