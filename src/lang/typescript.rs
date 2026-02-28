/// Returns true if the extracted lines represent a TypeScript overload stub
/// (or `.d.ts` declaration) rather than a real implementation body.
///
/// The definitive signal: a real implementation always contains `{`.
/// Overload signatures and abstract declarations have no body brace.
#[must_use]
pub fn is_overload_stub(lines: &[&str]) -> bool {
    !lines.iter().any(|l| l.contains('{'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_has_no_brace() {
        assert!(is_overload_stub(&["function foo(x: number): string;"]));
    }

    #[test]
    fn implementation_has_brace() {
        assert!(!is_overload_stub(&["function foo(x: number): string {", "  return x.toString();", "}"]));
    }

    #[test]
    fn empty_is_stub() {
        assert!(is_overload_stub(&[]));
    }
}
