//go:build !harness

package master

// mintInput is the identity in the shipped helper; the harness build salts it
// per test instance (uidsalt_harness.go).
func mintInput(gameUID string) string { return gameUID }
