pub fn is_big(x: i32) -> bool {
    x + 1 > 3
}

#[cfg(test)]
mod tests {
    use super::is_big;

    #[test]
    fn boundary() {
        assert!(!is_big(2));
        assert!(is_big(3));
    }
}
