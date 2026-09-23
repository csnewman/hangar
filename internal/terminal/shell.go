package terminal

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
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

		shell := loginShellOf(u.Username)
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

		if strconv.Itoa(os.Geteuid()) != u.Uid {
			cred, err := credential(u)
			if err != nil {
				return nil, err
			}
			cmd.SysProcAttr = &syscall.SysProcAttr{Credential: cred}
		}
		return cmd, nil
	}
}

// credential is who a user's shell runs as, with every group they are in:
// without the supplementary groups a user in docker's group could not reach
// Docker from their own terminal.
func credential(u *user.User) (*syscall.Credential, error) {
	uid, err := strconv.ParseUint(u.Uid, 10, 32)
	if err != nil {
		return nil, err
	}
	gid, err := strconv.ParseUint(u.Gid, 10, 32)
	if err != nil {
		return nil, err
	}
	cred := &syscall.Credential{Uid: uint32(uid), Gid: uint32(gid)}
	if ids, err := u.GroupIds(); err == nil {
		for _, id := range ids {
			if g, err := strconv.ParseUint(id, 10, 32); err == nil {
				cred.Groups = append(cred.Groups, uint32(g))
			}
		}
	}
	return cred, nil
}

// loginShellOf reads a user's shell from /etc/passwd, which os/user does
// not report, and falls back to bash or sh.
func loginShellOf(name string) string {
	if f, err := os.Open("/etc/passwd"); err == nil {
		defer f.Close()
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			fields := strings.Split(sc.Text(), ":")
			if len(fields) == 7 && fields[0] == name && fields[6] != "" {
				if _, err := os.Stat(fields[6]); err == nil && !strings.HasSuffix(fields[6], "nologin") {
					return fields[6]
				}
			}
		}
	}
	for _, sh := range []string{"/bin/bash", "/bin/sh"} {
		if _, err := os.Stat(sh); err == nil {
			return sh
		}
	}
	return "/bin/sh"
}
