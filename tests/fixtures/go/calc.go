package calc

// Add returns the sum of a and b.
func Add(a, b int) int {
	return a + b
}

// IsPositive returns true if n > 0.
func IsPositive(n int) bool {
	if n > 0 {
		return true
	}
	return false
}

// Max returns the larger of a and b.
func Max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

// Abs returns the absolute value of n.
func Abs(n int) int {
	if n < 0 {
		return -n
	}
	return n
}
