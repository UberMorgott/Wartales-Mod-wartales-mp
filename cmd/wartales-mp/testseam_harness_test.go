//go:build harness

package main

import (
	"io"
	"log"
	"testing"
)

func TestTestEndpointLoopback(t *testing.T) {
	logger := log.New(io.Discard, "", 0)
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "")
	if _, ok := testEndpoint(14251, logger); ok {
		t.Fatal("seam active without WARTALES_MP_TEST_INSTANCE")
	}
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "2")
	f, ok := testEndpoint(14252, logger)
	if !ok {
		t.Fatal("seam inactive with WARTALES_MP_TEST_INSTANCE=2")
	}
	ep, err := f()
	if err != nil || ep.Addr != "127.0.0.1:14252" || !ep.Verified {
		t.Fatalf("endpoint %+v, %v", ep, err)
	}
}
