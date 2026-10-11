//! A channel is a dependency-closed set of immutable patch handles.
//! Admission never chooses a winner among concurrent edits. Materialization
//! preserves those edits as explicit conflicts in the transport layer.
//! Handles identify immutable, full SHA-256-addressed records in the registry;
//! they are not truncated hashes. Approval and CI are read from that registry.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeError {
    Unapproved,
    ChecksFailed,
    MissingDependency,
}

/// Add exactly one reviewed patch. Duplicate deliveries are idempotent.
/// The caller must authenticate, bind the handle to its immutable record,
/// and supply that record's dependencies and review/check facts.
pub fn merge(
    channel: &Vec<u64>,
    dependencies: &Vec<u64>,
    approved: bool,
    checks_passed: bool,
    patch_id: u64,
) -> Result<Vec<u64>, MergeError> {
    if !approved {
        return Err(MergeError::Unapproved);
    }
    if !checks_passed {
        return Err(MergeError::ChecksFailed);
    }
    if !h5i_app_std::set::subset(dependencies, channel) {
        return Err(MergeError::MissingDependency);
    }
    Ok(h5i_app_std::set::insert(channel, &patch_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guards_and_duplicate_delivery() {
        assert_eq!(
            merge(&vec![], &vec![], false, true, 1),
            Err(MergeError::Unapproved)
        );
        assert_eq!(
            merge(&vec![], &vec![], true, false, 1),
            Err(MergeError::ChecksFailed)
        );
        assert_eq!(
            merge(&vec![], &vec![1], true, true, 2),
            Err(MergeError::MissingDependency)
        );
        assert_eq!(merge(&vec![1], &vec![1], true, true, 1), Ok(vec![1]));
    }
    #[test]
    fn ten_thousand_independent_agents_converge() {
        let mut forward = vec![];
        let mut reverse = vec![];
        for id in 1..=10_000 {
            forward = merge(&forward, &vec![], true, true, id).unwrap();
            reverse = merge(&reverse, &vec![], true, true, 10_001 - id).unwrap();
        }
        reverse.sort_unstable();
        assert_eq!(forward, reverse);
    }
}
