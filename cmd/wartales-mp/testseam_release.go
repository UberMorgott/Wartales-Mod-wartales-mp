//go:build !harness

package main

import (
	"log"

	"github.com/UberMorgott/wartales-mp/internal/master"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/punch"
)

// testEndpoint: the shipped helper has no harness seam (testseam_harness.go).
func testEndpoint(int, *log.Logger) (func() (nat.Endpoint, error), bool) { return nil, false }

// testPunch: the shipped helper has no harness seam (testseam_harness.go).
func testPunch(*punch.Socket, *master.Options, *log.Logger) {}
