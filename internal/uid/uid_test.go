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

func TestMintIsSessionAndStable(t *testing.T) {
	a, b := Mint("S0011223344556677"), Mint("S0011223344556677")
	if a != b || !IsSession(a) || IsSteam(a) {
		t.Fatalf("Mint = %q / %q", a, b)
	}
	if Ensure(a) != a || Ensure("S0011223344556677") != a {
		t.Fatal("Ensure must keep a Session id and mint anything else")
	}
}
