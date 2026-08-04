package calc

import "testing"

// Deliberately weak tests — only tests happy path, misses edge cases

func TestAdd(t *testing.T) {
	if Add(2, 3) != 5 {
		t.Error("expected 5")
	}
}

func TestIsPositive(t *testing.T) {
	if !IsPositive(1) {
		t.Error("expected true for 1")
	}
	// Missing: test for 0 and negative numbers!
}

func TestMax(t *testing.T) {
	if Max(3, 5) != 5 {
		t.Error("expected 5")
	}
	// Missing: test for equal values, test for a > b!
}

// Missing: TestAbs entirely!

func TestSum(t *testing.T) {
	if Sum(2, 3) != 5 {
		t.Error("expected 5")
	}
}
