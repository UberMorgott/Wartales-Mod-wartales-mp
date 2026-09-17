// Package install performs the one-off machine setup: a private CA trusted by
// Windows, a leaf certificate for master.shirogames.com, and hosts entries
// pointing those names at 127.0.0.1. Everything it does is reversible.
package install

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
)

// Install generates the certificates, trusts the CA and redirects the master
// hosts. Safe to run repeatedly.
//
// Writes to out are progress text: a failing console is not a reason to abort
// the setup, so their errors are discarded on purpose.
func Install(out io.Writer) error {
	dir := DataDir()
	if err := EnsureCerts(dir); err != nil {
		return fmt.Errorf("certificates: %w", err)
	}
	_, _ = fmt.Fprintf(out, "certificates in %s\n", dir)

	if err := addCAToRootStore(dir); err != nil {
		return fmt.Errorf("trust store: %w (run as administrator)", err)
	}
	_, _ = fmt.Fprintf(out, "CA installed in LocalMachine\\Root\n")

	if err := AddHostsEntries(); err != nil {
		return fmt.Errorf("hosts file: %w (run as administrator)", err)
	}
	_, _ = fmt.Fprintf(out, "hosts entries added to %s\n", HostsPath())
	return nil
}

// Uninstall reverses Install. Missing pieces are not an error. As in Install,
// errors from the progress writes to out are discarded on purpose.
func Uninstall(out io.Writer) error {
	var problems []string
	if err := RemoveHostsEntries(); err != nil {
		problems = append(problems, "hosts file: "+err.Error())
	} else {
		_, _ = fmt.Fprintf(out, "hosts entries removed from %s\n", HostsPath())
	}

	dir := DataDir()
	if thumb, err := CAThumbprint(dir); err == nil {
		if err := runCertutil("-delstore", "Root", thumb); err != nil {
			problems = append(problems, "trust store: "+err.Error())
		} else {
			_, _ = fmt.Fprintf(out, "CA removed from LocalMachine\\Root\n")
		}
	}
	if err := os.RemoveAll(dir); err != nil {
		problems = append(problems, "data dir: "+err.Error())
	} else {
		_, _ = fmt.Fprintf(out, "removed %s\n", dir)
	}

	if len(problems) > 0 {
		return fmt.Errorf("%s", strings.Join(problems, "; "))
	}
	return nil
}

// addCAToRootStore is idempotent: certutil -addstore -f replaces an identical
// certificate instead of adding a duplicate.
func addCAToRootStore(dir string) error {
	return runCertutil("-addstore", "-f", "Root", CACertPath(dir))
}

func runCertutil(args ...string) error {
	cmd := exec.Command("certutil", args...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("certutil %s: %v: %s", strings.Join(args, " "), err, strings.TrimSpace(string(out)))
	}
	return nil
}
