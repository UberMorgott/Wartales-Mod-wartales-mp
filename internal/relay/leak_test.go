package relay

import (
	"testing"

	"go.uber.org/goleak"
)

// The helper runs for as long as the game does, so a goroutine that outlives
// the connection it served is a real leak, not a test artefact.
func TestMain(m *testing.M) {
	goleak.VerifyTestMain(m)
}
