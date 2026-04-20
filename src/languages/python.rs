use crate::languages::LanguageSupport;

pub struct Python;

impl LanguageSupport for Python {
    fn name(&self) -> &str {
        "python"
    }

    fn extensions(&self) -> &[&str] {
        &["py"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn binary_expression_node(&self) -> &str {
        "binary_operator"
    }

    fn if_statement_node(&self) -> &str {
        "if_statement"
    }

    fn boolean_true_literals(&self) -> &[&str] {
        &["True"]
    }

    fn boolean_false_literals(&self) -> &[&str] {
        &["False"]
    }

    fn return_statement_node(&self) -> &str {
        "return_statement"
    }

    fn operator_field(&self) -> &str {
        "operator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_extension_detection() {
        let py = Python;
        assert_eq!(py.extensions(), &["py"]);
        assert_eq!(py.name(), "python");
    }

    #[test]
    fn test_python_parse_binary_expression() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "x = a + b\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        // Walk tree to find binary_operator node
        let mut found = false;
        let mut cursor = root.walk();
        fn walk(cursor: &mut tree_sitter::TreeCursor, target: &str, found: &mut bool) {
            if cursor.node().kind() == target {
                *found = true;
                return;
            }
            if cursor.goto_first_child() {
                loop {
                    walk(cursor, target, found);
                    if *found || !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        }
        walk(&mut cursor, py.binary_expression_node(), &mut found);
        assert!(found, "Expected to find '{}' node in Python AST", py.binary_expression_node());
    }

    #[test]
    fn test_python_parse_if_statement() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "if x < 10:\n    pass\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        fn walk(cursor: &mut tree_sitter::TreeCursor, target: &str, found: &mut bool) {
            if cursor.node().kind() == target {
                *found = true;
                return;
            }
            if cursor.goto_first_child() {
                loop {
                    walk(cursor, target, found);
                    if *found || !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        }
        walk(&mut cursor, py.if_statement_node(), &mut found);
        assert!(found, "Expected to find '{}' node in Python AST", py.if_statement_node());
    }

    #[test]
    fn test_python_parse_function() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "def add(a, b):\n    return a + b\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        // Verify we can find return_statement and binary_operator
        let mut found_return = false;
        let mut found_binary = false;
        let mut cursor = root.walk();
        fn walk(cursor: &mut tree_sitter::TreeCursor, ret: &str, bin: &str, fr: &mut bool, fb: &mut bool) {
            let kind = cursor.node().kind();
            if kind == ret { *fr = true; }
            if kind == bin { *fb = true; }
            if cursor.goto_first_child() {
                loop {
                    walk(cursor, ret, bin, fr, fb);
                    if (*fr && *fb) || !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        }
        walk(&mut cursor, py.return_statement_node(), py.binary_expression_node(), &mut found_return, &mut found_binary);
        assert!(found_return, "Expected return_statement node");
        assert!(found_binary, "Expected binary_operator node");
    }
}
