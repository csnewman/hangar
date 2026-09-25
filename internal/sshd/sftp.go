package sshd

import (
	"io"
	"os"

	"github.com/pkg/sftp"
)

// ServeSFTP serves SFTP on stdin and stdout until the client goes, as
// OpenSSH's sftp-server does, with the permissions of whoever runs it.
func ServeSFTP() error {
	srv, err := sftp.NewServer(stdio{}, sftp.WithServerWorkingDirectory(os.Getenv("HOME")))
	if err != nil {
		return err
	}
	if err := srv.Serve(); err != nil && err != io.EOF {
		return err
	}
	return nil
}

type stdio struct{}

func (stdio) Read(p []byte) (int, error)  { return os.Stdin.Read(p) }
func (stdio) Write(p []byte) (int, error) { return os.Stdout.Write(p) }
func (stdio) Close() error                { return os.Stdout.Close() }
