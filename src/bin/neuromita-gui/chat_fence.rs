/// An asynchronous chat UI result belongs to one particular selection, not
/// merely to a character that might be selected again later.
pub fn accepts(
    result_character: i32,
    result_revision: u64,
    selected_character: i32,
    selected_revision: u64,
) -> bool {
    result_character == selected_character && result_revision == selected_revision
}

#[cfg(test)]
mod tests {
    use super::accepts;

    #[test]
    fn late_attachment_does_not_clear_another_character_or_reselected_origin() {
        assert!(accepts(0, 0, 0, 0));
        assert!(!accepts(0, 0, 1, 1));
        assert!(!accepts(0, 0, 0, 2));
        assert!(accepts(0, 2, 0, 2));
    }

    #[test]
    fn identical_payloads_are_not_selection_provenance() {
        assert!(!accepts(1, 2, 1, 3));
        assert!(!accepts(2, 2, 1, 2));
        assert!(accepts(1, 3, 1, 3));
    }
}
