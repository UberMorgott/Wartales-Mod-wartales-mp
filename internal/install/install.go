// Package install performs the one-off per-user setup: a private CA and a leaf
// certificate for master.shirogames.com, both under %ProgramData%.
//
// Nothing here touches the machine. The CA is not added to the Windows trust
// store and no hosts entry is written: the game reaches our master because the
// libhl.dll shim resolves master*.shirogames.com to 127.0.0.1, and it trusts
// our certificate because the ssl.hdll shim appends this CA to the chain the
// game itself configures. Both shims live in the game folder and are installed
// by install.bat. No administrator rights are involved at any point.
package install

import (
	"fmt"
	"io"
	"os"
)

// Install generates the certificates the master listener and the ssl shim
// need. Safe to run repeatedly.
//
// Writes to out are progress text: a failing console is not a reason to abort
// the setup, so their errors are discarded on purpose.
func Install(out io.Writer) error {
	dir := DataDir()
	if err := EnsureCerts(dir); err != nil {
		return fmt.Errorf("certificates: %w", err)
	}
	_, _ = fmt.Fprintf(out, "certificates in %s\n", dir)
	_, _ = fmt.Fprintf(out, "the ssl.hdll shim trusts %s\n", CACertPath(dir))
	return nil
}

// Uninstall removes the generated certificates. A missing data directory is
// not an error. As in Install, errors from the progress writes are discarded
// on purpose.
func Uninstall(out io.Writer) error {
	dir := DataDir()
	if err := os.RemoveAll(dir); err != nil {
		return fmt.Errorf("data dir: %w", err)
	}
	_, _ = fmt.Fprintf(out, "removed %s\n", dir)
	return nil
}
