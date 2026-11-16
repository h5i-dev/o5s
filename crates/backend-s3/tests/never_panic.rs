//! Never-panic property tests for the S3 XML response parsers.
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn xml_total(d in proptest::collection::vec(any::<u8>(), 0..400)) {
        o5s_backend_s3::fuzzing::xml(&d);
    }

    #[test]
    fn xml_structured(tags in proptest::collection::vec("(ListBucketResult|Contents|Key|Size|IsTruncated|NextContinuationToken|Error|Code|UploadId|EncodingType)", 0..8),
                      text in "[a-z0-9%&;<>/ ]{0,20}") {
        let mut s = String::new();
        for t in &tags { s.push_str(&format!("<{t}>")); }
        s.push_str(&text);
        for t in tags.iter().rev() { s.push_str(&format!("</{t}>")); }
        o5s_backend_s3::fuzzing::xml(s.as_bytes());
    }
}
