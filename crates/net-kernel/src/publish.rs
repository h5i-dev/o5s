//! Publish decisions of the package and artifact fronts (PyPI, npm, Maven, OCI, HF). The shell
//! gathers the facts (what is stored, whether bodies match their digests); the rules are here.
//! Specs and proofs: `verify/net/proofs/Publish.lean`.

/// What a name already holds, compared with what a publish brings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    Absent,
    /// The same content (a re-upload of identical bytes).
    Same,
    /// Different content.
    Different,
}

/// A create-once publish (PyPI files, npm versions, Maven release files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateOnce {
    /// Create the name.
    Create,
    /// Already published with these bytes: succeed without writing.
    Unchanged,
    /// Published with other bytes: refuse (409). Release files never change.
    Conflict,
}

/// The create-once rule.
pub fn decide_create_once(e: Existing) -> CreateOnce {
    match e {
        Existing::Absent => CreateOnce::Create,
        Existing::Same => CreateOnce::Unchanged,
        Existing::Different => CreateOnce::Conflict,
    }
}

/// Why an OCI manifest push is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestNg {
    /// `PUT .../manifests/sha256:<d>` whose body does not hash to `<d>`.
    DigestInvalid,
    /// A referenced blob (config, layer) is not in the repository.
    BlobUnknown,
    /// The reference is neither a valid tag nor a digest.
    TagInvalid,
}

/// An OCI manifest push: the reference must be a valid tag or the body's own digest, and every
/// blob it references must be present.
pub fn decide_manifest(
    ref_is_digest: bool,
    digest_matches: bool,
    tag_valid: bool,
    missing_blobs: u64,
) -> Result<(), ManifestNg> {
    if ref_is_digest {
        if !digest_matches {
            return Err(ManifestNg::DigestInvalid);
        }
    } else if !tag_valid {
        return Err(ManifestNg::TagInvalid);
    }
    if missing_blobs > 0 {
        return Err(ManifestNg::BlobUnknown);
    }
    Ok(())
}

/// Why an HF commit is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitNg {
    /// `parentCommit` is not the branch head.
    StaleParent,
    /// A path is invalid (empty, absolute, `..`, too long).
    BadPath,
    /// An LFS file was not uploaded (or with another size).
    MissingLfs,
}

fn same_opt(a: &Option<Vec<u8>>, b: &Option<Vec<u8>>) -> bool {
    match a {
        Some(x) => match b {
            Some(y) => *x == *y,
            None => false,
        },
        None => match b {
            Some(_) => false,
            None => true,
        },
    }
}

/// An HF commit: a given parent must be the head; all paths valid; all LFS files uploaded with
/// their declared sizes.
pub fn decide_commit(
    parent: &Option<Vec<u8>>,
    head: &Option<Vec<u8>>,
    paths_ok: bool,
    lfs_ok: bool,
) -> Result<(), CommitNg> {
    match parent {
        Some(_) => {
            if !same_opt(parent, head) {
                return Err(CommitNg::StaleParent);
            }
        }
        None => {}
    }
    if !paths_ok {
        return Err(CommitNg::BadPath);
    }
    if !lfs_ok {
        return Err(CommitNg::MissingLfs);
    }
    Ok(())
}

/// An npm write that names a CouchDB revision (`/-rev/<rev>`): it applies only to that revision.
pub fn decide_rev(given: &Vec<u8>, current: &Vec<u8>) -> bool {
    *given == *current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert_eq!(decide_create_once(Existing::Absent), CreateOnce::Create);
        assert_eq!(decide_create_once(Existing::Same), CreateOnce::Unchanged);
        assert_eq!(decide_create_once(Existing::Different), CreateOnce::Conflict);
        assert_eq!(decide_manifest(true, true, false, 0), Ok(()));
        assert_eq!(decide_manifest(true, false, true, 0), Err(ManifestNg::DigestInvalid));
        assert_eq!(decide_manifest(false, false, false, 0), Err(ManifestNg::TagInvalid));
        assert_eq!(decide_manifest(false, false, true, 2), Err(ManifestNg::BlobUnknown));
        let h = Some(b"c1".to_vec());
        assert_eq!(decide_commit(&None, &h, true, true), Ok(()));
        assert_eq!(decide_commit(&h, &h, true, true), Ok(()));
        assert_eq!(decide_commit(&Some(b"c0".to_vec()), &h, true, true), Err(CommitNg::StaleParent));
        assert_eq!(decide_commit(&None, &h, false, true), Err(CommitNg::BadPath));
        assert_eq!(decide_commit(&None, &h, true, false), Err(CommitNg::MissingLfs));
        assert!(decide_rev(&b"1-a".to_vec(), &b"1-a".to_vec()));
        assert!(!decide_rev(&b"1-a".to_vec(), &b"2-b".to_vec()));
    }
}
