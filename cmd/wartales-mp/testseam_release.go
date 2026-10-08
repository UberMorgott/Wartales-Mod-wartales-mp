//go:build !harness

package main

import (
	"log"

	"github.com/UberMorgott/wartales-mp/internal/nat"
)

// testEndpoint: the shipped helper has no harness seam (testseam_harness.go).
func testEndpoint(int, *log.Logger) (func() (nat.Endpoint, error), bool) { return nil, false }
