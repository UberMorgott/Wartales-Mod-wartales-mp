//go:build harness

package master

import "os"

// mintInput salts the game's id with the harness instance (test build only,
// WMP_TEST_SEAM): two local games on one Steam account must not log in as
// the same player, or the guest takes the host's lobby slot.
func mintInput(gameUID string) string {
	if n := os.Getenv("WARTALES_MP_TEST_INSTANCE"); n != "" {
		return gameUID + "#t" + n
	}
	return gameUID
}
