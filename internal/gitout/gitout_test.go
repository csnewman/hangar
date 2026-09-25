package gitout_test

import (
	"testing"

	"github.com/csnewman/hangar/internal/gitout"
)

func TestProgress(t *testing.T) {
	out := "Cloning into 'x.cloning'...\nremote: Enumerating objects: 2742, done.\n" +
		"Receiving objects:  12% (330/2742)\rReceiving objects:  45% (1234/2742), 1.20 MiB | 2.00 MiB/s\r"
	stage, done, total, ok := gitout.Progress(out)
	if !ok || stage != "Receiving objects" || done != 1234 || total != 2742 {
		t.Fatalf("got %q %d/%d %v", stage, done, total, ok)
	}
	out += "Receiving objects: 100% (2742/2742), done.\nResolving deltas:  10% (80/800)\r"
	if stage, done, total, _ := gitout.Progress(out); stage != "Resolving deltas" || done != 80 || total != 800 {
		t.Fatalf("got %q %d/%d", stage, done, total)
	}
	if _, _, _, ok := gitout.Progress("Cloning into 'x'...\n"); ok {
		t.Fatal("progress found where there is none")
	}
}

func TestFailure(t *testing.T) {
	for out, want := range map[string]string{
		"Cloning into '/workspace/app.cloning'...\n" +
			"Warning: Permanently added 'github.com' (ED25519) to the list of known hosts.\n" +
			"git@github.com: Permission denied (publickey).\n" +
			"fatal: Could not read from remote repository.\n\n" +
			"Please make sure you have the correct access rights\nand the repository exists.\n": "git@github.com: Permission denied (publickey).",
		"Cloning into 'x'...\nHost key verification failed.\nfatal: Could not read from remote repository.\n\n" +
			"Please make sure you have the correct access rights\nand the repository exists.\n": "Host key verification failed.",
		"Cloning into 'x'...\nremote: Repository not found.\nfatal: repository 'https://github.com/a/b.git/' not found\n": "fatal: repository 'https://github.com/a/b.git/' not found",
		"": "no output",
	} {
		if got := gitout.Failure(out); got != want {
			t.Errorf("Failure(%q) = %q, want %q", out, got, want)
		}
	}
}
