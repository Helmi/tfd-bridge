/// Placeholder for future TFD Bridge capability modules.
///
/// This crate will host OS-agnostic logic for the local replay bridge,
/// file watching, and wows-toolkit integration in follow-up tasks.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
