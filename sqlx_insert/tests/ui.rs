#[cfg(test)]
mod tests {
    #[cfg(not(feature = "use-macros"))]
    #[test]
    fn ui() {
        let t = trybuild::TestCases::new();

        t.compile_fail("tests/ui/not_compatible_fields.rs");
        t.compile_fail("tests/ui/unnamed_fields.rs");
        t.compile_fail("tests/ui/unknown_database.rs");
        t.compile_fail("tests/ui/unsupported_types.rs");
    }
}
