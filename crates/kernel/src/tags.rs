//! Deletion tags cancel independently: restoring one tag does not cancel another.

/// Each covering tag must index `restored`. A vertex is visible only when every
/// tag covering it has been restored. Dense identifiers and the flag table are
/// supplied by the document adapter; neither changes replicated operation IDs.
pub fn visible(covering: &Vec<usize>, restored: &Vec<bool>) -> bool {
    let mut visible = true;
    let mut i = 0;
    while i < covering.len() {
        if !restored[covering[i]] {
            visible = false;
        }
        i += 1;
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::visible;

    #[test]
    fn restoring_one_tag_does_not_cancel_another() {
        assert!(visible(&vec![], &vec![]));
        assert!(!visible(&vec![0, 1], &vec![false, false]));
        assert!(!visible(&vec![0, 1], &vec![true, false]));
        assert!(visible(&vec![0, 1], &vec![true, true]));
        assert!(!visible(&vec![0, 1, 2], &vec![true, true, false]));
    }
}
