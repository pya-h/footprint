use sha2::{Digest, Sha256};

/// Hashes a string using SHA256 and formats it as `<sha256:16hex>`.
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let hex = hex::encode(hasher.finalize());
    format!("<sha256:{}>", &hex[..16])
}

/// Hashes the userinfo, every query value and the fragment of a URL; the host and path stay.
///
/// Example:
/// `https://me:pw@example.com/api?user=john#access_token=x` ->
/// `https://<sha256:…>@example.com/api?user=<sha256:…>#<sha256:…>`
pub fn redact_url(url: &str) -> String {
    let (url, fragment) = match url.split_once('#') {
        Some((u, f)) => (u, Some(f)),
        None => (url, None),
    };
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let mut result = redact_userinfo(base);
    if let Some(query) = query {
        let params: Vec<String> = query
            .split('&')
            .map(|param| match param.split_once('=') {
                Some((k, v)) => format!("{k}={}", hash_secret(v)),
                None if !param.is_empty() => hash_secret(param),
                None => String::new(),
            })
            .collect();
        result.push('?');
        result.push_str(&params.join("&"));
    }
    if let Some(fragment) = fragment.filter(|f| !f.is_empty()) {
        result.push('#');
        result.push_str(&hash_secret(fragment));
    }
    result
}

/// `scheme://user:pass@host/…` keeps the scheme and host, hashes `user:pass`.
fn redact_userinfo(base: &str) -> String {
    let Some((scheme, rest)) = base.split_once("://") else {
        return base.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rsplit_once('@') {
        Some((userinfo, host)) => format!(
            "{scheme}://{}@{host}{}",
            hash_secret(userinfo),
            &rest[authority_end..]
        ),
        None => base.to_string(),
    }
}

/// Redacts sensitive URLs and credentials in free-form text.
pub fn redact_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while pos < text.len() {
        let remainder = &text[pos..];
        let http_idx = remainder.find("http://");
        let https_idx = remainder.find("https://");

        let next_url = match (http_idx, https_idx) {
            (Some(h), Some(s)) => Some(std::cmp::min(h, s)),
            (Some(h), None) => Some(h),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        };

        if let Some(offset) = next_url {
            let start = pos + offset;
            result.push_str(&text[pos..start]);

            // Find end of URL (whitespace, quotes, closing parens/brackets)
            let url_remainder = &text[start..];
            let end_offset = url_remainder
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '>')
                .unwrap_or(url_remainder.len());

            let raw_url = &url_remainder[..end_offset];
            result.push_str(&redact_url(raw_url));
            pos = start + end_offset;
        } else {
            result.push_str(remainder);
            break;
        }
    }

    result
}

/// Recursively redacts sensitive values and URLs within a JSON structure.
pub fn redact_json(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                let lower_k = k.to_lowercase();
                if lower_k.contains("password")
                    || lower_k.contains("secret")
                    || lower_k.contains("token")
                    || lower_k.contains("auth")
                    || lower_k.contains("cookie")
                {
                    match v {
                        serde_json::Value::String(s) => {
                            new_map.insert(k.clone(), serde_json::Value::String(hash_secret(s)));
                        }
                        _ => {
                            new_map.insert(
                                k.clone(),
                                serde_json::Value::String(hash_secret(&v.to_string())),
                            );
                        }
                    }
                } else {
                    new_map.insert(k.clone(), redact_json(v));
                }
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            let new_arr = arr.iter().map(redact_json).collect();
            serde_json::Value::Array(new_arr)
        }
        serde_json::Value::String(s) => serde_json::Value::String(redact_text(s)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_url() {
        let plain = "https://example.com/path";
        assert_eq!(redact_url(plain), plain);

        let with_params = "https://zoom.us/download?code=12345&name=alice#install";
        let redacted = redact_url(with_params);
        assert!(!redacted.contains("12345"));
        assert!(!redacted.contains("alice"));
        assert!(redacted.starts_with("https://zoom.us/download?code=<sha256:"));
        assert!(redacted.contains("&name=<sha256:"));
        assert!(!redacted.ends_with("#install"));

        let token = redact_url("https://me:hunter2@app.example/cb#access_token=abc.def");
        assert!(token.starts_with("https://<sha256:"), "{token}");
        assert!(token.contains("@app.example/cb#<sha256:"), "{token}");
        assert!(
            !token.contains("hunter2") && !token.contains("abc.def"),
            "{token}"
        );
    }

    #[test]
    fn test_redact_text() {
        let text = "User visited https://us09-web-zoom.com/j/123456?pwd=secret and logged in";
        let redacted = redact_text(text);
        assert!(!redacted.contains("secret"));
        assert!(redacted.contains("https://us09-web-zoom.com/j/123456?pwd=<sha256:"));
        assert!(redacted.ends_with("and logged in"));
    }

    #[test]
    fn test_redact_json() {
        let v = serde_json::json!({
            "url": "https://example.com/login?token=abc",
            "password": "my_password",
            "user": "paya",
            "nested": {
                "auth_bearer": "xyz"
            }
        });
        let r = redact_json(&v);
        assert!(!r["url"].as_str().unwrap().contains("abc"));
        assert_ne!(r["password"].as_str().unwrap(), "my_password");
        assert!(r["password"].as_str().unwrap().starts_with("<sha256:"));
        assert_eq!(r["user"].as_str().unwrap(), "paya");
        assert!(
            r["nested"]["auth_bearer"]
                .as_str()
                .unwrap()
                .starts_with("<sha256:")
        );
    }
}
