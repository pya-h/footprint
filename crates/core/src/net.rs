//! Network helpers shared across collectors and correlation.

/// Extracts a clean lowercase host or domain from a URL, authority, or hostname string.
/// Strips scheme (`http://`, `https://`, etc.), userinfo (`user:pass@`), port (`:8080`),
/// brackets around IPv6 addresses (`[::1]`), and paths.
/// Returns None for non-network schemes (`about:`, `file:`, `data:`) or non-host strings.
pub fn extract_host(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let rest = match s.split_once("://") {
        Some((scheme, r)) => {
            let scheme_lower = scheme.to_ascii_lowercase();
            if scheme_lower == "file" || scheme_lower == "about" || scheme_lower == "data" {
                return None;
            }
            r
        }
        None => {
            if s.starts_with('/') || s.contains(' ') {
                return None;
            }
            s
        }
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host_port.split([':', ',']).next()?,
    };
    (!host.is_empty() && (host.contains('.') || host == "localhost" || host.contains(':')))
        .then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_various_hosts() {
        assert_eq!(
            extract_host("https://us09-web-zoom.com/j/123").as_deref(),
            Some("us09-web-zoom.com")
        );
        assert_eq!(
            extract_host("https://User:Secret@Zoom.US:443/x?a=b").as_deref(),
            Some("zoom.us")
        );
        assert_eq!(
            extract_host("https://me.simuul.xyz:443,*").as_deref(),
            Some("me.simuul.xyz")
        );
        assert_eq!(extract_host("http://[::1]:8080/").as_deref(), Some("::1"));
        assert_eq!(extract_host("zoom.us").as_deref(), Some("zoom.us"));
        assert_eq!(extract_host("about:blank"), None);
        assert_eq!(extract_host("file:///home/u/x"), None);
        assert_eq!(extract_host("sudo apt install ./zoom.deb"), None);
        assert_eq!(extract_host("/tmp/evil.sh"), None);
    }
}
