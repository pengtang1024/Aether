//! Minimal AWS Signature Version 4 signer for S3 GET/HEAD.

use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use url::Url;

type HmacSha256 = Hmac<Sha256>;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub struct Credentials<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub session_token: Option<&'a str>,
    pub region: &'a str,
}

pub fn sign(method: &str, url: &Url, credentials: &Credentials<'_>) -> Result<String, String> {
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();

    let host = url
        .host_str()
        .ok_or_else(|| "S3 URL has no host".to_string())?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let canonical_uri = url.path();

    let mut canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{EMPTY_SHA256}\nx-amz-date:{amz_date}\n");
    let mut signed_headers = "host;x-amz-content-sha256;x-amz-date".to_string();
    if let Some(token) = credentials.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{EMPTY_SHA256}"
    );
    let scope = format!("{date}/{}/s3/aws4_request", credentials.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, credentials.region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    ))
}

pub fn sign_headers(
    method: &str,
    url: &Url,
    credentials: &Credentials<'_>,
) -> Result<Vec<(&'static str, String)>, String> {
    let now = Utc::now();
    let mut headers = vec![
        ("x-amz-date", now.format("%Y%m%dT%H%M%SZ").to_string()),
        ("x-amz-content-sha256", EMPTY_SHA256.to_string()),
        ("authorization", sign(method, url, credentials)?),
    ];
    if let Some(token) = credentials.session_token {
        headers.push(("x-amz-security-token", token.to_string()));
    }
    Ok(headers)
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_deterministic_and_contains_scope() {
        let url = Url::parse("https://bucket.s3.us-east-1.amazonaws.com/key").unwrap();
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE",
            secret_key: "secret",
            session_token: None,
            region: "us-east-1",
        };
        let first = sign("GET", &url, &credentials).unwrap();
        let second = sign("GET", &url, &credentials).unwrap();
        assert!(first.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(first.contains("/s3/aws4_request"));
        assert_eq!(first.len(), second.len());
    }
}
