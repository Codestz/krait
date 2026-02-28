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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_pointer_receiver() {
        assert_eq!(base_name("(*knowledgeService).CreateKnowledgeFromFile"), "CreateKnowledgeFromFile");
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
}
