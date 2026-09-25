// Package gitout reads what git writes to its error stream: the progress
// of a clone or fetch, and why one failed.
package gitout

import (
	"regexp"
	"strconv"
	"strings"
)

// progressLine is a line of git's progress: "Receiving objects:  45%
// (1234/2742)".
var progressLine = regexp.MustCompile(`^(Receiving objects|Resolving deltas|Updating files):\s+\d+% \((\d+)/(\d+)\)`)

// advice is what git adds after the cause of a failed fetch, the same
// whatever the cause was.
var advice = regexp.MustCompile(`^(Please make sure you have the correct access rights|and the repository exists\.|fatal: Could not read from remote repository\.)$`)

// lines splits git's output into lines. Progress rewrites its line with
// carriage returns, so those end lines too.
func lines(output string) []string {
	return strings.FieldsFunc(output, func(r rune) bool { return r == '\r' || r == '\n' })
}

// Progress is the last progress line in output, as far as it goes: its
// stage ("Receiving objects"), and how many of how many are done.
func Progress(output string) (stage string, done, total int64, ok bool) {
	ls := lines(output)
	for i := len(ls) - 1; i >= 0; i-- {
		if g := progressLine.FindStringSubmatch(strings.TrimSpace(ls[i])); g != nil {
			done, _ = strconv.ParseInt(g[2], 10, 64)
			total, _ = strconv.ParseInt(g[3], 10, 64)
			return g[1], done, total, true
		}
	}
	return "", 0, 0, false
}

// Failure is the line of output that says why git failed: the last one
// that is neither progress, nor what it was doing, nor its closing advice.
func Failure(output string) string {
	ls := lines(output)
	for i := len(ls) - 1; i >= 0; i-- {
		l := strings.TrimSpace(ls[i])
		if l == "" || advice.MatchString(l) || progressLine.MatchString(l) || strings.HasPrefix(l, "Cloning into") {
			continue
		}
		return l
	}
	return "no output"
}
