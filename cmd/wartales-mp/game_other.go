//go:build !windows

package main

import "fmt"

// gameExe is the process wartales-mp lives and dies with.
const gameExe = "Wartales.exe"

// watchGame is Windows-only: the shims that start wartales-mp are Windows DLLs.
// Elsewhere the server can still be run with -no-watch.
func watchGame() (uint32, <-chan struct{}, error) {
	return 0, nil, fmt.Errorf("watching %s is only supported on Windows, use -no-watch", gameExe)
}
