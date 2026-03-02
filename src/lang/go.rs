/// Strip Go method receiver prefix from a symbol name.
///
/// Go struct methods are indexed with a receiver prefix:
///   `"(*ReceiverType).MethodName"` or `"ReceiverType.MethodName"`
///
/// Returns the base name (everything after the last `")."`), or the original
/// string if no receiver prefix is present.
#[must_use]
pub fn base_name(s: &str) -> &str {
    s.rfind(").").map_or(s, |i| &s[i + 2..])
}

/// Check if a gopls document symbol name matches a `Receiver.Method` query.
///
/// gopls returns Go receiver methods as flat document symbol entries with names
/// like `(*Handler).CreateSession` (pointer receiver) or `(Handler).CreateSession`
/// (value receiver) — NOT as children of the struct.
///
/// Returns `true` when the symbol's receiver type (with `*` stripped) equals
/// `receiver` and `base_name(symbol_name)` equals `method`.
#[must_use]
pub fn receiver_method_matches(symbol_name: &str, receiver: &str, method: &str) -> bool {
    if base_name(symbol_name) != method {
        return false;
    }
    // symbol_name has form "(*Receiver).Method" or "(Receiver).Method"
    if let Some(paren_end) = symbol_name.find(").") {
        let receiver_part = &symbol_name[1..paren_end]; // strip leading "("
        let receiver_type = receiver_part.trim_start_matches('*');
        return receiver_type == receiver;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_pointer_receiver() {
        assert_eq!(
            base_name("(*knowledgeService).CreateKnowledgeFromFile"),
            "CreateKnowledgeFromFile"
        );
    }

    #[test]
    fn strips_value_receiver() {
        assert_eq!(base_name("(userRepo).Save"), "Save");
    }

    #[test]
    fn passthrough_no_receiver() {
        assert_eq!(base_name("ProcessUsers"), "ProcessUsers");
    }

    #[test]
    fn passthrough_empty() {
        assert_eq!(base_name(""), "");
    }

    #[test]
    fn receiver_method_matches_pointer() {
        assert!(receiver_method_matches(
            "(*Handler).CreateSession",
            "Handler",
            "CreateSession"
        ));
    }

    #[test]
    fn receiver_method_matches_value() {
        assert!(receiver_method_matches(
            "(userRepo).Save",
            "userRepo",
            "Save"
        ));
    }

    #[test]
    fn receiver_method_wrong_method() {
        assert!(!receiver_method_matches(
            "(*Handler).CreateSession",
            "Handler",
            "DeleteSession"
        ));
    }

    #[test]
    fn receiver_method_wrong_receiver() {
        assert!(!receiver_method_matches(
            "(*Handler).CreateSession",
            "Router",
            "CreateSession"
        ));
    }

    #[test]
    fn receiver_method_no_receiver_prefix() {
        assert!(!receiver_method_matches("ProcessUsers", "ProcessUsers", ""));
    }
}
