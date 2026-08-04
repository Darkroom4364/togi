package scale

import "testing"

// Deliberately thin tests: fast, deterministic, and far from exhaustive.
// The scale corpus measures runner behavior, not fixture test quality.

func TestDouble(t *testing.T) {
	if Double(3) != 6 {
		t.Error("expected 6")
	}
}

func TestSign(t *testing.T) {
	if Sign(5) != 1 {
		t.Error("expected 1 for positive input")
	}
	// Missing: zero and negative inputs.
}
