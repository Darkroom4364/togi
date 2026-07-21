package calc

import "testing"

// Deliberately weak: only the n=1 case, never 0 or negatives.
func TestIsPositive(t *testing.T) {
	if !IsPositive(1) {
		t.Fatal("1 should be positive")
	}
}
