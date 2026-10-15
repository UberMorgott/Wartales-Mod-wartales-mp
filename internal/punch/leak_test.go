package punch

import (
	"testing"

	"go.uber.org/goleak"
)

// A closed socket must leave nothing running but kcp-go's process-wide
// packet scheduler (started in its init).
func TestMain(m *testing.M) {
	goleak.VerifyTestMain(m,
		goleak.IgnoreTopFunction("github.com/xtaci/kcp-go/v5.(*TimedSched).sched"),
		goleak.IgnoreTopFunction("github.com/xtaci/kcp-go/v5.(*TimedSched).prepend"))
}
