//go:build harness

package master

import "testing"

// A harness direct lobby names a member by its game's own id; SDR keeps the
// Steam id, and a member whose game reported nothing keeps its Session id.
func TestHarnessMemberID(t *testing.T) {
	t.Setenv("WARTALES_MP_TEST_INSTANCE", "2")
	guest := &session{uid: "Xminted", game: "X29f792402"}
	if got := (&lobby{transport: TransportDirect}).idOf(guest); got != "X29f792402" {
		t.Fatalf("direct: idOf = %q, want the game's id", got)
	}
	host := &session{uid: "Xminted", game: "S9f792402", steam: "S9f792402"}
	if got := (&lobby{transport: TransportSDR}).idOf(host); got != "S9f792402" {
		t.Fatalf("SDR: idOf = %q, want the Steam id", got)
	}
	if got := (&lobby{transport: TransportDirect}).idOf(&session{uid: "Xminted"}); got != "Xminted" {
		t.Fatalf("no game id: idOf = %q, want the Session id", got)
	}
}
