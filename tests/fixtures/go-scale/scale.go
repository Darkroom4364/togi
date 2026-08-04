package scale

// Double returns twice n.
func Double(n int) int {
	return n * 2
}

// Sign returns 1 for positive n, -1 otherwise.
func Sign(n int) int {
	if n > 0 {
		return 1
	}
	return -1
}
