package calc

// IsPositive reports whether n is greater than zero.
func IsPositive(n int) bool {
	return n > 0
}

// Max returns the larger of a and b.
func Max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
