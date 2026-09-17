// Command wartales-mp replaces Wartales' networking rendezvous: it serves the
// game a local stand-in for master.shirogames.com and relays the traffic
// directly between players, with neither Shiro's master nor Steam in the path.
//
//	wartales-mp install     one-off machine setup (admin)
//	wartales-mp uninstall   undo it
//	wartales-mp run         serve and launch the game (default)
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

  wartales-mp install             generate the CA, trust it, redirect the master hosts (admin)
  wartales-mp uninstall           undo the above
  wartales-mp run [flags]         run master + relay and launch the game
  wartales-mp code encode IP:PORT print the join code for an endpoint
  wartales-mp code decode CODE    print the endpoint behind a join code

run flags:
  -port N        public TCP port for relay and proxy-link (default 14250)
  -master ADDR   master listen address (default 127.0.0.1:60442)
  -game PATH     path to Wartales.exe (autodetected)
  -no-game       do not launch the game, just serve
`)
}
