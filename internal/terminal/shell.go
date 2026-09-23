package terminal

import (
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"

	"github.com/csnewman/hangar/internal/sysuser"
)

// LoginShell returns a Shell that starts a login shell for a request's user,
// or for fallback when the request names none. A fallback that does not
// exist on this machine means whoever runs the Manager.
//
// The shell is given the environment a terminal program would set -- TERM
// and COLORTERM, so programs use colour and the features the browser's
// terminal has -- and nothing of the Manager's own.
func LoginShell(fallback string) Shell {
	return func(req Request) (*exec.Cmd, error) {
		name := req.User
		if name == "" {
			name = fallback
		}
		u, err := user.Lookup(name)
		if err != nil {
			if req.User != "" {
				return nil, fmt.Errorf("no user %q", req.User)
			}
			if u, err = user.Current(); err != nil {
				return nil, err
			}
		}

		shell := sysuser.LoginShell(u.Username)
		dir := req.Dir
		if dir == "" {
			dir = u.HomeDir
		}
		if st, err := os.Stat(dir); err != nil || !st.IsDir() {
			dir = u.HomeDir
		}

		// argv[0] starting with "-" is how a shell knows it is a login
		// shell, and so reads the profile that sets PATH and the prompt.
		cmd := exec.Command(shell)
		cmd.Args = []string{"-" + filepath.Base(shell)}
		cmd.Dir = dir
		cmd.Env = []string{
			"HOME=" + u.HomeDir,
			"USER=" + u.Username,
			"LOGNAME=" + u.Username,
			"SHELL=" + shell,
			"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
			"TERM=xterm-256color",
			"COLORTERM=truecolor",
			"TERM_PROGRAM=hangar",
			"LANG=C.UTF-8",
		}

		if err := sysuser.RunAs(cmd, u); err != nil {
			return nil, err
		}
		return cmd, nil
	}
}
