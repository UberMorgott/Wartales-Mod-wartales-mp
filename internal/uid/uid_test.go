package uid

import "testing"

func TestIsSteam(t *testing.T) {
	good := []string{"S0011223344556677", "S0123456789abcdef", "S0123456789ABCDEF"}
	for _, id := range good {
		if !IsSteam(id) {
			t.Errorf("IsSteam(%q) = false, want true", id)
		}
	}
	bad := []string{"", "S", "S00112233445566", "S00112233445566778", "X0011223344556677", "Snot-a-steam-id!!",
		Mint("S0011223344556677")}
	for _, id := range bad {
		if IsSteam(id) {
			t.Errorf("IsSteam(%q) = true, want false", id)
		}
	}
}

func TestSteamID64(t *testing.T) {
	// Account 12345678 (0x00bc614e): little-endian bytes 4e 61 bc 00, and the
	// high dword 0x01100001 xors to zero, which is how the game prints it.
	const id = "S4e61bc0000000000"
	got, ok := SteamID64(id)
	if !ok || got != 0x0110000100BC614E {
		t.Fatalf("SteamID64(%q) = %x, %v", id, got, ok)
	}
	if FromSteamID64(got) != id {
		t.Fatalf("FromSteamID64(%x) = %q", got, FromSteamID64(got))
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
