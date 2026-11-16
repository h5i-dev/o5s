//! Minimal parsing of the S3 XML responses we use. Unknown elements are ignored.
//! Malformed input yields `S3Error::Protocol`, never a panic.

use crate::error::{S3Error, S3Result};
use quick_xml::Reader;
use quick_xml::events::Event;

/// Every text node as `(element path joined by '/', text)`, in document order.
/// The root element is included in the path (e.g. `ListBucketResult/Contents/Key`).
fn flatten(xml: &[u8]) -> S3Result<Vec<(String, String)>> {
    let mut r = Reader::from_reader(xml);
    r.config_mut().trim_text(false);
    let mut path: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut buf = Vec::new();
    // True while consecutive text/entity events belong to the same text node.
    let mut in_text = false;
    // Appends to the current text node or starts a new one.
    let push = |out: &mut Vec<(String, String)>, in_text: bool, p: String, t: &str| match out.last_mut() {
        Some((lp, lt)) if in_text && *lp == p => lt.push_str(t),
        _ => out.push((p, t.to_string())),
    };
    loop {
        let was_text = in_text;
        in_text = false;
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned()),
            Ok(Event::End(_)) => {
                if path.pop().is_none() {
                    return Err(S3Error::Protocol("unbalanced XML".into()));
                }
            }
            Ok(Event::Empty(e)) => {
                // `<Tag/>`: an element with empty text.
                let mut p = path.join("/");
                if !p.is_empty() {
                    p.push('/');
                }
                p.push_str(&String::from_utf8_lossy(e.local_name().as_ref()));
                out.push((p, String::new()));
            }
            Ok(Event::Text(t)) => {
                let s = t.decode().map_err(|e| S3Error::Protocol(format!("bad XML text: {e}")))?;
                let s =
                    quick_xml::escape::unescape(&s).map_err(|e| S3Error::Protocol(format!("bad XML escape: {e}")))?;
                if !path.is_empty() {
                    push(&mut out, was_text, path.join("/"), &s);
                    in_text = true;
                }
            }
            Ok(Event::GeneralRef(g)) => {
                // quick-xml reports entity refs (`&amp;`) separately; append to the last text node.
                let name = String::from_utf8_lossy(g.as_ref()).into_owned();
                let ch = match name.as_str() {
                    "amp" => "&".to_string(),
                    "lt" => "<".to_string(),
                    "gt" => ">".to_string(),
                    "quot" => "\"".to_string(),
                    "apos" => "'".to_string(),
                    n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                        .map(String::from)
                        .ok_or_else(|| S3Error::Protocol(format!("bad char ref {n}")))?,
                    n if n.starts_with('#') => n[1..]
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                        .map(String::from)
                        .ok_or_else(|| S3Error::Protocol(format!("bad char ref {n}")))?,
                    n => return Err(S3Error::Protocol(format!("unknown XML entity {n}"))),
                };
                push(&mut out, was_text, path.join("/"), &ch);
                in_text = true;
            }
            Ok(Event::CData(t)) => out.push((path.join("/"), String::from_utf8_lossy(&t).into_owned())),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(S3Error::Protocol(format!("bad XML: {e}"))),
        }
        buf.clear();
    }
    // Drop whitespace-only formatting nodes between elements.
    out.retain(|(_, t)| !t.trim().is_empty());
    Ok(out)
}

/// Text of the first element named `tag` (at any depth).
pub fn find_text(xml: &[u8], tag: &str) -> Option<String> {
    flatten(xml).ok()?.into_iter().find(|(p, _)| p.rsplit('/').next() == Some(tag)).map(|(_, t)| t)
}

/// `<Error><Code>…</Code><Message>…</Message></Error>` → `(code, message)`.
pub fn parse_error(xml: &[u8]) -> Option<(String, String)> {
    let f = flatten(xml).ok()?;
    let code = f.iter().find(|(p, _)| p == "Error/Code").map(|(_, t)| t.clone())?;
    let msg = f.iter().find(|(p, _)| p == "Error/Message").map(|(_, t)| t.clone()).unwrap_or_default();
    Some((code, msg))
}

/// One page of `ListObjectsV2`.
#[derive(Debug, Default, PartialEq)]
pub struct ListPage {
    /// `(key, size)`, keys already URL-decoded if `EncodingType=url` was used.
    pub objects: Vec<(String, u64)>,
    pub next_token: Option<String>,
}

pub fn parse_list(xml: &[u8]) -> S3Result<ListPage> {
    let f = flatten(xml)?;
    let mut page = ListPage::default();
    let url_encoded = f.iter().any(|(p, t)| p == "ListBucketResult/EncodingType" && t == "url");
    let mut truncated = false;
    let mut cur_key: Option<String> = None;
    for (p, t) in f {
        match p.as_str() {
            "ListBucketResult/IsTruncated" => truncated = t.trim() == "true",
            "ListBucketResult/NextContinuationToken" => page.next_token = Some(t),
            "ListBucketResult/Contents/Key" => {
                cur_key = Some(if url_encoded { url_decode(&t)? } else { t });
            }
            "ListBucketResult/Contents/Size" => {
                let size: u64 = t.trim().parse().map_err(|_| S3Error::Protocol(format!("bad Size {t:?}")))?;
                let key = cur_key.take().ok_or_else(|| S3Error::Protocol("Size before Key in listing".into()))?;
                page.objects.push((key, size));
            }
            _ => {}
        }
    }
    if cur_key.is_some() {
        return Err(S3Error::Protocol("listing entry without Size".into()));
    }
    if truncated && page.next_token.is_none() {
        return Err(S3Error::Protocol("truncated listing without NextContinuationToken".into()));
    }
    if !truncated {
        page.next_token = None;
    }
    Ok(page)
}

/// `InitiateMultipartUploadResult/UploadId`.
pub fn parse_upload_id(xml: &[u8]) -> S3Result<String> {
    flatten(xml)?
        .into_iter()
        .find(|(p, _)| p == "InitiateMultipartUploadResult/UploadId")
        .map(|(_, t)| t)
        .ok_or_else(|| S3Error::Protocol("no UploadId in CreateMultipartUpload response".into()))
}

/// What a 200 response to `CompleteMultipartUpload` contained. S3 may return
/// 200 with an `<Error>` body when the failure happens after headers were sent.
pub enum CompleteResult {
    Ok,
    Error { code: String, message: String },
}

pub fn parse_complete(xml: &[u8]) -> S3Result<CompleteResult> {
    if let Some((code, message)) = parse_error(xml) {
        return Ok(CompleteResult::Error { code, message });
    }
    let f = flatten(xml)?;
    if f.iter().any(|(p, _)| p.starts_with("CompleteMultipartUploadResult")) {
        Ok(CompleteResult::Ok)
    } else {
        Err(S3Error::Protocol("unexpected CompleteMultipartUpload response".into()))
    }
}

/// Builds the `CompleteMultipartUpload` request body.
pub fn complete_body(parts: &[(u32, String)]) -> String {
    let mut s = String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for (n, etag) in parts {
        s.push_str(&format!(
            "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
            quick_xml::escape::escape(etag.as_str())
        ));
    }
    s.push_str("</CompleteMultipartUpload>");
    s
}

fn url_decode(s: &str) -> S3Result<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or_else(|| S3Error::Protocol(format!("bad %-escape in {s:?}")))?;
                out.push(h);
                i += 3;
            }
            b'%' => return Err(S3Error::Protocol(format!("truncated %-escape in {s:?}"))),
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| S3Error::Protocol(format!("non-UTF-8 key {s:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_v2() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><Prefix>p/</Prefix>
<KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated>
<NextContinuationToken>tok&amp;1</NextContinuationToken>
<Contents><Key>p/a%20b</Key><LastModified>x</LastModified><ETag>&quot;e&quot;</ETag><Size>12</Size></Contents>
<Contents><Key>p/c</Key><Size>0</Size></Contents></ListBucketResult>"#;
        let p = parse_list(xml).unwrap();
        assert_eq!(p.objects, vec![("p/a b".to_string(), 12), ("p/c".to_string(), 0)]);
        assert_eq!(p.next_token.as_deref(), Some("tok&1"));
    }

    #[test]
    fn list_empty() {
        let xml = br#"<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>"#;
        assert_eq!(parse_list(xml).unwrap(), ListPage::default());
    }

    #[test]
    fn errors() {
        let xml = br#"<?xml version="1.0"?><Error><Code>SlowDown</Code><Message>Please reduce</Message></Error>"#;
        assert_eq!(parse_error(xml), Some(("SlowDown".into(), "Please reduce".into())));
        assert_eq!(parse_error(b"garbage<<"), None);
        assert!(matches!(parse_complete(xml).unwrap(), CompleteResult::Error { .. }));
    }

    #[test]
    fn malformed_never_panics() {
        for x in [&b"<a><b></a>"[..], b"</x>", b"<ListBucketResult><Contents><Size>zz</Size></Contents>", b"\xff\xfe"] {
            let _ = parse_list(x);
            let _ = parse_upload_id(x);
            let _ = parse_complete(x);
            let _ = parse_error(x);
        }
        assert!(url_decode("%").is_err());
        assert!(url_decode("%4").is_err());
        assert!(url_decode("%zz").is_err());
        assert_eq!(url_decode("a%2Fb").unwrap(), "a/b");
    }

    #[test]
    fn upload_id() {
        let xml = br#"<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key><UploadId>abc-123</UploadId></InitiateMultipartUploadResult>"#;
        assert_eq!(parse_upload_id(xml).unwrap(), "abc-123");
    }
}
