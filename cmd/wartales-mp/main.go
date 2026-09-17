// Command wartales-mp replaces Wartales' networking rendezvous: it serves the
// game a local stand-in for master.shirogames.com and relays the traffic
// directly between players, with neither Shiro's master nor Steam in the path.
//
// It is started by the libhl.dll shim from inside the game folder and exits
// with the game. Nothing about the machine is modified: no hosts entry, no
// certificate in the Windows store, no administrator rights.
//
//	wartales-mp install     generate the certificates (run does this too)
//	wartales-mp uninstall   delete them again
//	wartales-mp run         serve until the game exits (default)
//	wartales-mp code ...    encode/decode a join code
package main

import (
	"fmt"
	"os"
)

func main() {
	args := os.Args[1:]
	cmd := "run"
	if len(args) > 0 && len(args[0]) > 0 && args[0][0] != '-' {
		cmd, args = args[0], args[1:]
	}

	var err error
	switch cmd {
	case "run":
		err = runCmd(args)
	case "install":
		err = installCmd()
	case "uninstall":
		err = uninstallCmd()
	case "code":
		err = codeCmd(args)
	case "help", "-h", "--help":
		usage()
	default:
		usage()
		err = fmt.Errorf("unknown command %q", cmd)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprint(os.Stderr, `wartales-mp - direct multiplayer for Wartales

  wartales-mp install             generate the local CA and master certificate
  wartales-mp uninstall           delete them again
  wartales-mp run [flags]         run master + relay, exit with the game
  wartales-mp code encode IP:PORT print the join code for an endpoint
  wartales-mp code decode CODE    print the endpoint behind a join code

run flags:
  -port N        public TCP port for relay and proxy-link (default 14250)
  -master ADDR   master listen address (default 127.0.0.1:60442)
  -no-watch      keep serving after the game exits

The shims in the game folder do the rest: install.bat puts them there.
`)
}
