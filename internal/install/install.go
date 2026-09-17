// Package install owns the private CA and the leaf certificate for
// master.shirogames.com, both under %ProgramData%, and the two console-facing
// helpers that tell a player how to turn the mod on and off.
//
// Nothing here touches the machine or the game folder. The CA is not added to
// the Windows trust store and no hosts entry is written: the game reaches our
// master because the winmm.dll proxy hooks hl_host_resolve so
// master*.shirogames.com resolves to 127.0.0.1, and it trusts our certificate
// because the same proxy hooks ssl_conf_set_ca and appends this CA to the chain
// the game itself configures. Installation is a single file: the player drops
// winmm.dll into the game folder, which the game loads by itself on launch. No
// administrator rights are involved at any point.
package install

import (
	"fmt"
	"io"
)

// ProxyFile is the one file installation consists of: the game loads it from
// its own folder because Windows searches the application directory before
// System32 for this (non-KnownDLL) name.
const ProxyFile = "winmm.dll"

// Install explains the one and only install step. There is nothing to do on
// this machine: the certificates are generated on demand by run, and turning
// the mod on is a single file copy the player performs in the game folder.
//
// Writes to out are progress text: a failing console is not a reason to fail,
// so their errors are discarded on purpose.
func Install(out io.Writer) error {
	_, _ = fmt.Fprintf(out, "To turn the mod on, copy this one file into the Wartales game folder:\n")
	_, _ = fmt.Fprintf(out, "    %s\n", ProxyFile)
	_, _ = fmt.Fprintf(out, "The game loads it by itself on the next launch. Nothing else is changed:\n")
	_, _ = fmt.Fprintf(out, "no rename, no hosts file, no certificate store, no Steam launch options.\n")
	return nil
}

// Uninstall explains the one and only uninstall step: delete that same file.
func Uninstall(out io.Writer) error {
	_, _ = fmt.Fprintf(out, "To turn the mod off, delete this one file from the Wartales game folder:\n")
	_, _ = fmt.Fprintf(out, "    %s\n", ProxyFile)
	_, _ = fmt.Fprintf(out, "The game reverts to vanilla networking on the next launch.\n")
	return nil
}
