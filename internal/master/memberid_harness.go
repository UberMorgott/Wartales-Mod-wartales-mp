//go:build harness

package master

import "os"

// harnessMemberID is the local co-op harness seam (test build only,
// WMP_TEST_SEAM, active with WARTALES_MP_TEST_INSTANCE set): a direct lobby
// renders a member by the id its own game calls itself (user/login "uid"), as
// the vanilla master does, instead of the minted Session id. Both harness
// games run on one Steam account, so the guest game calls itself by a Session
// id (patcher harness.rs, harnessUid) and the lobby is not Steam-only. Without
// it a guest never finds itself, nor the host by its save's playerId, in a
// loaded save's lobby (docs/coop-harness-plan.md, slice 2 findings).
func harnessMemberID(p Peer) string {
	if os.Getenv("WARTALES_MP_TEST_INSTANCE") == "" {
		return ""
	}
	return p.GameID()
}
