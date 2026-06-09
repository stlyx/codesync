use std::collections::HashMap;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub fn verify_webhook_secret(
    secret: Option<&str>,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> bool {
    let Some(secret) = secret else {
        return true;
    };
    let secret_bytes = secret.as_bytes();

    if get_headers(headers, "X-CodeSync-Token")
        .into_iter()
        .any(|token| constant_time_eq(token.as_bytes(), secret_bytes))
    {
        return true;
    }

    if get_headers(headers, "Authorization")
        .into_iter()
        .any(|authorization| {
            authorization
                .strip_prefix("Bearer ")
                .is_some_and(|token| constant_time_eq(token.as_bytes(), secret_bytes))
        })
    {
        return true;
    }

    let expected_signature = signature_for(secret, body);
    if get_headers(headers, "X-Hub-Signature-256")
        .into_iter()
        .any(|signature| constant_time_eq(signature.as_bytes(), expected_signature.as_bytes()))
    {
        return true;
    }

    false
}

pub fn signature_for(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any size");
    mac.update(body);
    let digest = mac.finalize().into_bytes();

    let mut signature = String::with_capacity("sha256=".len() + digest.len() * 2);
    signature.push_str("sha256=");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(signature, "{byte:02x}");
    }
    signature
}

fn get_headers<'a>(headers: &'a HashMap<String, String>, name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn allows_when_no_secret() {
        let headers = header_map(&[]);

        assert!(verify_webhook_secret(None, &headers, br#"{"ok":true}"#));
    }

    #[test]
    fn accepts_token_header() {
        let headers = header_map(&[("X-CodeSync-Token", "shared-secret")]);

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn accepts_bearer_header() {
        let headers = header_map(&[("Authorization", "Bearer shared-secret")]);

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn accepts_github_hmac_header() {
        let body = br#"{"ref":"refs/heads/master"}"#;
        let signature = signature_for("shared-secret", body);
        let headers = header_map(&[("X-Hub-Signature-256", signature.as_str())]);

        assert!(verify_webhook_secret(Some("shared-secret"), &headers, body));
    }

    #[test]
    fn rejects_wrong_secret() {
        let headers = header_map(&[("X-CodeSync-Token", "wrong-secret")]);

        assert!(!verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let headers = header_map(&[("authorization", "Bearer shared-secret")]);

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn rejects_wrong_hmac_body() {
        let signed_body = br#"{"ref":"refs/heads/master"}"#;
        let actual_body = br#"{"ref":"refs/heads/main"}"#;
        let signature = signature_for("shared-secret", signed_body);
        let headers = header_map(&[("X-Hub-Signature-256", signature.as_str())]);

        assert!(!verify_webhook_secret(Some("shared-secret"), &headers, actual_body));
    }

    #[test]
    fn continues_after_wrong_token_header_to_valid_hmac() {
        let body = br#"{"ref":"refs/heads/master"}"#;
        let signature = signature_for("shared-secret", body);
        let headers = header_map(&[
            ("X-CodeSync-Token", "wrong-secret"),
            ("X-Hub-Signature-256", signature.as_str()),
        ]);

        assert!(verify_webhook_secret(Some("shared-secret"), &headers, body));
    }

    #[test]
    fn continues_after_wrong_token_header_to_valid_bearer() {
        let headers = header_map(&[
            ("X-CodeSync-Token", "wrong-secret"),
            ("Authorization", "Bearer shared-secret"),
        ]);

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn duplicate_case_headers_do_not_make_auth_nondeterministic() {
        let mut headers = header_map(&[
            ("Authorization", ""),
            ("authorization", ""),
        ]);
        place_wrong_value_on_first_matching_header(
            &mut headers,
            "Authorization",
            "Bearer wrong-secret",
            "Bearer shared-secret",
        );

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    #[test]
    fn duplicate_case_token_headers_do_not_make_auth_nondeterministic() {
        let mut headers = header_map(&[
            ("X-CodeSync-Token", ""),
            ("x-codesync-token", ""),
        ]);
        place_wrong_value_on_first_matching_header(
            &mut headers,
            "X-CodeSync-Token",
            "wrong-secret",
            "shared-secret",
        );

        assert!(verify_webhook_secret(
            Some("shared-secret"),
            &headers,
            br#"{"ok":true}"#
        ));
    }

    fn place_wrong_value_on_first_matching_header(
        headers: &mut HashMap<String, String>,
        logical_name: &str,
        wrong_value: &str,
        valid_value: &str,
    ) {
        let first_matching_key = headers
            .keys()
            .find(|key| key.eq_ignore_ascii_case(logical_name))
            .expect("matching header should exist")
            .clone();

        for (key, value) in headers {
            if key.eq_ignore_ascii_case(logical_name) {
                *value = if *key == first_matching_key {
                    wrong_value.to_string()
                } else {
                    valid_value.to_string()
                };
            }
        }
    }
}
