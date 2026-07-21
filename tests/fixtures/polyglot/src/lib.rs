pub fn clamp_nonnegative(x: i32) -> i32 {
    if x < 0 { 0 } else { x }
}

pub fn sign(x: i32) -> i32 {
    if x > 0 {
        1
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    // Deliberately weak: only a positive input, never the x < 0 branch.
    #[test]
    fn clamp_positive_keeps_value() {
        assert_eq!(super::clamp_nonnegative(5), 5);
    }
}
