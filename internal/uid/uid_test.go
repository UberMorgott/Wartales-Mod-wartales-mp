package uid

import "testing"

func TestIsSteam(t *testing.T) {
	// The game strips trailing zeros (UserID.hx:117): a live id is short.
	good := []string{"S9f792402", "S4e61bc", "S0011223344556677", "S0123456789abcdef", "S0123456789ABCDEF", "S1"}
	for _, id := range good {
		if !IsSteam(id) {
			t.Errorf("IsSteam(%q) = false, want true", id)
		}
	}
	bad := []string{"", "S", "S00112233445566778", "X0011223344556677", "Snot-a-steam-id!!", "S9f79240g",
		Mint("S0011223344556677")}
	for _, id := range bad {
		if IsSteam(id) {
			t.Errorf("IsSteam(%q) = true, want false", id)
		}
	}
}

func TestSteamID64(t *testing.T) {
	// Account 12345678 (0x00bc614e): little-endian bytes 4e 61 bc 00, the
	// high dword 0x01100001 xors to zero, and the game prints it without the
	// trailing zeros.
	const id = "S4e61bc"
	got, ok := SteamID64(id)
	if !ok || got != 0x0110000100BC614E {
		t.Fatalf("SteamID64(%q) = %x, %v", id, got, ok)
	}
	if FromSteamID64(got) != id {
		t.Fatalf("FromSteamID64(%x) = %q", got, FromSteamID64(got))
	}
	// The padded spelling names the same account.
	if full, ok := SteamID64("S4e61bc0000000000"); !ok || full != got {
		t.Fatalf("SteamID64(padded) = %x, %v", full, ok)
	}
	// The id seen in a live log: account 0x0224799f.
	if live, ok := SteamID64("S9f792402"); !ok || live != 0x011000010224799f {
		t.Fatalf("SteamID64(live) = %x, %v", live, ok)
	}
	if _, ok := SteamID64("X4e61bc0000000000"); ok {
		t.Fatal("a Session id has no SteamID64")
	}
}

func TestMintIsSessionAndStable(t *testing.T) {
	a, b := Mint("S0011223344556677"), Mint("S0011223344556677")
	if a != b || !IsSession(a) || IsSteam(a) {
		t.Fatalf("Mint = %q / %q", a, b)
	}
	if Ensure(a) != a || Ensure("S0011223344556677") != a {
		t.Fatal("Ensure must keep a Session id and mint anything else")
	}
}
