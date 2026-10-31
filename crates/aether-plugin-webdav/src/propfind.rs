//! Minimal WebDAV `PROPFIND` client and XML parser.
//!
//! Only the handful of DAV properties the Host needs for a probe are
//! extracted. Parsing is hand-written so the plugin does not pull in an XML
//! crate: no DTDs are processed, unknown entities are preserved literally and
//! malformed documents produce an error instead of panicking.

use std::fmt;

use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use reqwest::Method;

/// Standard `Depth: 0` allprop-style request body.
pub const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getcontentlength/>
    <D:getlastmodified/>
    <D:getetag/>
    <D:resourcetype/>
  </D:prop>
</D:propfind>"#;

/// DAV properties relevant to a probe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavProps {
    pub size: Option<u64>,
    pub etag: Option<String>,
    /// Raw `getlastmodified` value; the SDK `Metadata` has no field for it yet.
    #[allow(dead_code)]
    pub modified: Option<String>,
    pub is_collection: bool,
    pub href: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropfindError {
    Request(String),
    Status(u16),
    MalformedXml(String),
}

impl fmt::Display for PropfindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(message) => write!(formatter, "PROPFIND request failed: {message}"),
            Self::Status(status) => write!(formatter, "PROPFIND returned HTTP {status}"),
            Self::MalformedXml(message) => write!(formatter, "invalid PROPFIND XML: {message}"),
        }
    }
}

impl std::error::Error for PropfindError {}

/// Send a `Depth: 0` PROPFIND and parse the Multi-Status response.
///
/// Any failure (non-2xx status, transport error, unparseable body) is returned
/// as an error so the caller can fall back to an HTTP HEAD probe.
pub fn propfind(client: &Client, url: &str) -> Result<DavProps, PropfindError> {
    let method = Method::from_bytes(b"PROPFIND")
        .map_err(|error| PropfindError::Request(error.to_string()))?;
    let response = client
        .request(method, url)
        .header("Depth", "0")
        .header(CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(PROPFIND_BODY)
        .send()
        .map_err(|error| PropfindError::Request(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(PropfindError::Status(status.as_u16()));
    }
    let body = response
        .text()
        .map_err(|error| PropfindError::Request(error.to_string()))?;
    parse_multistatus(&body)
}

/// Parse a DAV Multi-Status document into the properties of its first
/// `response` element. Namespace prefixes (`D:`, `d:`, `lp1:`, none) are
/// ignored by comparing local names only.
pub fn parse_multistatus(body: &str) -> Result<DavProps, PropfindError> {
    let document = parse_document(body)?;
    let response = find_first(&document, "response")
        .ok_or_else(|| malformed("document has no DAV response element"))?;

    let mut props = DavProps {
        href: child_text(response, "href")
            .map(str::trim)
            .filter(|href| !href.is_empty())
            .map(ToOwned::to_owned),
        ..DavProps::default()
    };

    let mut saw_prop = false;
    for propstat in children_named(response, "propstat") {
        if !status_is_success(child_text(propstat, "status")) {
            continue;
        }
        if let Some(prop) = find_first(propstat, "prop") {
            saw_prop = true;
            collect_prop(prop, &mut props);
        }
    }
    if !saw_prop {
        if let Some(prop) = children_named(response, "prop").next() {
            saw_prop = true;
            collect_prop(prop, &mut props);
        }
    }
    if !saw_prop {
        return Err(malformed("document has no DAV prop element"));
    }
    Ok(props)
}

fn collect_prop(prop: &Element, props: &mut DavProps) {
    for child in &prop.children {
        let text = child.text.trim();
        if eq_name(&child.name, "getcontentlength") {
            // A missing or non-numeric length is not fatal: the file is still
            // a file, the size is simply unknown.
            if let Ok(size) = text.parse::<u64>() {
                props.size = Some(size);
            }
        } else if eq_name(&child.name, "getetag") {
            if !text.is_empty() {
                props.etag = Some(text.to_owned());
            }
        } else if eq_name(&child.name, "getlastmodified") {
            if !text.is_empty() {
                props.modified = Some(text.to_owned());
            }
        } else if eq_name(&child.name, "resourcetype") {
            props.is_collection |= child
                .children
                .iter()
                .any(|value| eq_name(&value.name, "collection"));
        }
    }
}

fn status_is_success(status: Option<&str>) -> bool {
    let Some(status) = status else {
        return true;
    };
    match status
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
    {
        Some(code) => (200..300).contains(&code),
        // A propstat without a recognizable status line is treated as a
        // normal (successful) property block.
        None => true,
    }
}

fn child_text<'a>(element: &'a Element, name: &str) -> Option<&'a str> {
    element
        .children
        .iter()
        .find(|child| eq_name(&child.name, name))
        .map(|child| child.text.as_str())
}

fn children_named<'a>(
    element: &'a Element,
    name: &'a str,
) -> impl Iterator<Item = &'a Element> + 'a {
    element
        .children
        .iter()
        .filter(move |child| eq_name(&child.name, name))
}

fn find_first<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    if eq_name(&element.name, name) {
        return Some(element);
    }
    element
        .children
        .iter()
        .find_map(|child| find_first(child, name))
}

fn eq_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

struct Element {
    /// Local name with any namespace prefix removed.
    name: String,
    text: String,
    children: Vec<Element>,
}

fn parse_document(input: &str) -> Result<Element, PropfindError> {
    let source = input.strip_prefix('\u{feff}').unwrap_or(input);
    let bytes = source.as_bytes();
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'<' {
            let start = index;
            while index < bytes.len() && bytes[index] != b'<' {
                index += 1;
            }
            let text = &source[start..index];
            match stack.last_mut() {
                Some(element) => element.text.push_str(&decode_entities(text)),
                None if text.trim().is_empty() => {}
                None => return Err(malformed("text outside the root element")),
            }
            continue;
        }

        let rest = &source[index..];
        if rest.starts_with("<?") {
            let end = rest
                .find("?>")
                .ok_or_else(|| malformed("unterminated processing instruction"))?;
            index += end + 2;
            continue;
        }
        if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .ok_or_else(|| malformed("unterminated comment"))?;
            index += end + 3;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest
                .find("]]>")
                .ok_or_else(|| malformed("unterminated CDATA section"))?;
            let text = &rest[9..end];
            let Some(element) = stack.last_mut() else {
                return Err(malformed("CDATA outside the root element"));
            };
            element.text.push_str(text);
            index += end + 3;
            continue;
        }
        if rest.starts_with("<!") {
            // DTDs are rejected outright: no entity expansion, no external
            // subset resolution.
            return Err(malformed("unsupported markup declaration"));
        }
        if rest.starts_with("</") {
            let end = rest
                .find('>')
                .ok_or_else(|| malformed("unterminated closing tag"))?;
            let raw_name = rest[2..end].trim();
            let name = local_name(raw_name);
            if name.is_empty() || !valid_name(raw_name) {
                return Err(malformed("invalid closing tag name"));
            }
            let Some(element) = stack.pop() else {
                return Err(malformed("unexpected closing tag"));
            };
            if !eq_name(&element.name, name) {
                return Err(malformed("mismatched closing tag"));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => {
                    if root.is_some() {
                        return Err(malformed("multiple root elements"));
                    }
                    root = Some(element);
                }
            }
            index += end + 1;
            continue;
        }

        let end = tag_end(rest)?;
        let inner = &rest[1..end];
        let trimmed = inner.trim_end();
        let self_closing = trimmed.ends_with('/');
        let inner = if self_closing {
            trimmed[..trimmed.len() - 1].trim_end()
        } else {
            inner
        };
        let name_end = inner
            .find(|character: char| character.is_whitespace())
            .unwrap_or(inner.len());
        let raw_name = &inner[..name_end];
        if raw_name.is_empty() || !valid_name(raw_name) {
            return Err(malformed("invalid opening tag name"));
        }
        let element = Element {
            name: local_name(raw_name).to_owned(),
            text: String::new(),
            children: Vec::new(),
        };
        if self_closing {
            match stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => {
                    if root.is_some() {
                        return Err(malformed("multiple root elements"));
                    }
                    root = Some(element);
                }
            }
        } else {
            stack.push(element);
        }
        index += end + 1;
    }

    if !stack.is_empty() {
        return Err(malformed("unclosed element"));
    }
    root.ok_or_else(|| malformed("empty document"))
}

fn tag_end(source: &str) -> Result<usize, PropfindError> {
    let bytes = source.as_bytes();
    let mut quote = None;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'>' => return Ok(index),
            _ => {}
        }
    }
    Err(malformed("unterminated tag"))
}

/// Strip a namespace prefix such as `D:` or `lp1:`.
fn local_name(raw: &str) -> &str {
    raw.rsplit_once(':').map(|(_, local)| local).unwrap_or(raw)
}

fn valid_name(raw: &str) -> bool {
    let mut characters = raw.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_') {
        return false;
    }
    characters
        .all(|character| character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | ':'))
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut decoded = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(position) = rest.find('&') {
        decoded.push_str(&rest[..position]);
        let after = &rest[position + 1..];
        let entity = after
            .find(';')
            .and_then(|end| decode_entity(&after[..end]).map(|value| (value, end)));
        match entity {
            Some((value, end)) => {
                decoded.push(value);
                rest = &after[end + 1..];
            }
            None => {
                decoded.push('&');
                rest = after;
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
            }
            entity
                .strip_prefix('#')
                .and_then(|number| number.parse::<u32>().ok())
                .and_then(char::from_u32)
        }
    }
}

fn malformed(message: &str) -> PropfindError {
    PropfindError::MalformedXml(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTISTATUS_PREFIXES: &[&str] = &[
        r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/file.bin</D:href><D:propstat><D:prop><D:getcontentlength>1234</D:getcontentlength><D:getlastmodified>Wed, 01 Jan 2025 00:00:00 GMT</D:getlastmodified><D:getetag>"abc"</D:getetag><D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#,
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/file.bin</d:href><d:propstat><d:prop><d:getcontentlength>1234</d:getcontentlength><d:getetag>"abc"</d:getetag><d:resourcetype></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        r#"<lp1:multistatus xmlns:lp1="DAV:"><lp1:response><lp1:href>/dav/file.bin</lp1:href><lp1:propstat><lp1:prop><lp1:getcontentlength>1234</lp1:getcontentlength><lp1:getetag>"abc"</lp1:getetag><lp1:resourcetype/></lp1:prop><lp1:status>HTTP/1.1 200 OK</lp1:status></lp1:propstat></lp1:response></lp1:multistatus>"#,
        r#"<multistatus xmlns="DAV:"><response><href>/dav/file.bin</href><propstat><prop><getcontentlength>1234</getcontentlength><getetag>"abc"</getetag><resourcetype/></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#,
    ];

    #[test]
    fn parses_every_namespace_prefix() {
        for document in MULTISTATUS_PREFIXES {
            let props = parse_multistatus(document).unwrap();
            assert_eq!(props.size, Some(1234), "{document}");
            assert_eq!(props.etag.as_deref(), Some("\"abc\""), "{document}");
            assert!(!props.is_collection, "{document}");
            assert_eq!(props.href.as_deref(), Some("/dav/file.bin"), "{document}");
        }
    }

    #[test]
    fn detects_collections() {
        let document = r#"<?xml version="1.0" encoding="utf-8"?>
            <D:multistatus xmlns:D="DAV:">
              <D:response>
                <D:href>/dav/folder/</D:href>
                <D:propstat>
                  <D:prop>
                    <D:resourcetype><D:collection/></D:resourcetype>
                    <D:getlastmodified>Wed, 01 Jan 2025 00:00:00 GMT</D:getlastmodified>
                  </D:prop>
                  <D:status>HTTP/1.1 200 OK</D:status>
                </D:propstat>
              </D:response>
            </D:multistatus>"#;
        let props = parse_multistatus(document).unwrap();
        assert!(props.is_collection);
        assert_eq!(props.size, None);
        assert_eq!(
            props.modified.as_deref(),
            Some("Wed, 01 Jan 2025 00:00:00 GMT")
        );
    }

    #[test]
    fn missing_content_length_is_not_an_error() {
        let document = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:propstat><D:prop><D:getetag>"only-etag"</D:getetag><D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;
        let props = parse_multistatus(document).unwrap();
        assert_eq!(props.size, None);
        assert_eq!(props.etag.as_deref(), Some("\"only-etag\""));
        assert!(!props.is_collection);
    }

    #[test]
    fn ignores_failed_propstat_blocks() {
        let document = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:propstat><D:prop><D:getcontentlength>99</D:getcontentlength></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat><D:propstat><D:prop><D:getetag>"ok"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;
        let props = parse_multistatus(document).unwrap();
        assert_eq!(props.size, None);
        assert_eq!(props.etag.as_deref(), Some("\"ok\""));
    }

    #[test]
    fn decodes_entities_without_expansion() {
        let document = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:propstat><D:prop><D:getetag>&quot;a&amp;b&quot;</D:getetag><D:getcontentlength>7</D:getcontentlength></D:prop></D:propstat></D:response></D:multistatus>"#;
        let props = parse_multistatus(document).unwrap();
        assert_eq!(props.etag.as_deref(), Some("\"a&b\""));
        assert_eq!(props.size, Some(7));
    }

    #[test]
    fn rejects_malformed_xml() {
        let cases = [
            "",
            "<D:multistatus>",
            "<D:multistatus></D:response>",
            "<D:multistatus><D:response></D:multistatus></D:response>",
            "<D:multistatus><D:response>",
            "<D:multistatus><D:response><D:prop><1:getetag>x</1:getetag></D:prop></D:response></D:multistatus>",
            "<!DOCTYPE foo><D:multistatus/>",
            "<D:multistatus><D:response><D:prop><D:getetag>unterminated</D:prop></D:response></D:multistatus>",
            "<D:multistatus><D:response><D:prop><D:getetag>a<b</D:getetag></D:prop></D:response></D:multistatus>",
        ];
        for document in cases {
            assert!(
                matches!(
                    parse_multistatus(document),
                    Err(PropfindError::MalformedXml(_))
                ),
                "expected error for {document:?}"
            );
        }
    }

    #[test]
    fn reports_non_success_status() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });

        let client = Client::builder().no_proxy().build().unwrap();
        let error = propfind(&client, &format!("http://{address}/dav/file.bin")).unwrap_err();
        assert_eq!(error, PropfindError::Status(405));
    }
}
