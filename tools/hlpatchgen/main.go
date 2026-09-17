// hlpatchgen writes the C header with the HashLink bytecode patch table that
// the winmm.dll proxy compiles in, so the table itself lives only in
// internal/hlpatch (where it is also tested against the real bytecode).
//
//	go run ./tools/hlpatchgen -out shim/proxy/hlpatch.h
package main

import (
	"flag"
	"fmt"
	"os"

	"github.com/UberMorgott/wartales-mp/internal/hlpatch"
)

func main() {
	out := flag.String("out", "", "header file to write")
	flag.Parse()
	if *out == "" {
		fmt.Fprintln(os.Stderr, "hlpatchgen: -out is required")
		os.Exit(2)
	}
	if err := os.WriteFile(*out, hlpatch.Header(), 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "hlpatchgen: %v\n", err)
		os.Exit(1)
	}
}
