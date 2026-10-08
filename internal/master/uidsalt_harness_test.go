//go:build harness

package master

import (
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// Two harness instances on one Steam account log in as two players.
func TestMintInputSaltsPerInstance(t *testing.T) {
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "1")
	a := uid.Mint(mintInput("S76561198000000000"))
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "2")
	b := uid.Mint(mintInput("S76561198000000000"))
	if a == b {
		t.Fatalf("instances 1 and 2 share uid %s", a)
	}
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "")
	if got := mintInput("S1"); got != "S1" {
		t.Fatalf("no instance: mintInput = %q, want S1", got)
	}
}
