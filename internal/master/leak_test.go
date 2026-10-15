package master

import (
	"testing"

	"go.uber.org/goleak"
)

// The helper runs for as long as the game does, so a goroutine that outlives
// the connection it served is a real leak, not a test artefact. kcp-go
// (internal/punch) starts its packet scheduler once per process, in init.
func TestMain(m *testing.M) {
	goleak.VerifyTestMain(m,
		goleak.IgnoreTopFunction("github.com/xtaci/kcp-go/v5.(*TimedSched).sched"),
		goleak.IgnoreTopFunction("github.com/xtaci/kcp-go/v5.(*TimedSched).prepend"))
}
