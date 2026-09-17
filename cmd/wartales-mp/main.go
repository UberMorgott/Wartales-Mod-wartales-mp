// Command wartales-mp replaces Wartales' networking rendezvous: it serves the
// game a local stand-in for master.shirogames.com and relays the traffic
// directly between players, with neither Shiro's master nor Steam in the path.
//
// It is embedded in the winmm.dll proxy, extracted to %LOCALAPPDATA% and
// started by it from inside the game process, and exits with the game. Nothing
// about the machine is modified: no hosts entry, no certificate in the Windows
// store, no administrator rights.
//
//	wartales-mp install     print the one-file install step
//	wartales-mp uninstall   print the one-file uninstall step
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

  wartales-mp install             print the one-file install step
  wartales-mp uninstall           print the one-file uninstall step
  wartales-mp run [flags]         run master + relay, exit with the game
  wartales-mp code encode IP:PORT print the join code for an endpoint
  wartales-mp code decode CODE    print the endpoint behind a join code

run flags:
  -port N        public TCP port for relay and proxy-link (default 14250)
  -master ADDR   master listen address (default 127.0.0.1:60442)
  -no-watch      keep serving after the game exits
  -transport M   auto (default), direct or sdr: how lobbies we host carry
                 the game; auto = direct relay when the public endpoint is
                 verified reachable, otherwise Steam Datagram Relay

The winmm.dll proxy in the game folder does the rest: it starts this helper and
hooks the game's name resolution and CA setup from inside the process.
`)
}
