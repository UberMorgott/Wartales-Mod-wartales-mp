//go:build !harness

package master

// harnessMemberID: the shipped helper renders minted ids (memberid_harness.go).
func harnessMemberID(Peer) string { return "" }
