//! Startup check against GitHub Releases for a build newer than this one.
//!
//! No auto-download/install — that needs Developer ID signing + notarization,
//! which this ad-hoc-signed build doesn't have. Just a small header badge
//! linking to the release page when the latest tag differs from ours.

const REPO: &str = "JUSTIVE/Graviz";

pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

/// Blocking network call — always run on a background thread, never the UI
/// thread. Any failure (offline, no releases published yet, rate-limited)
/// just yields `None`; this is a nice-to-have, not something to surface as
/// an error.
pub fn check_for_update() -> Option<UpdateInfo> {
    let body: serde_json::Value = ureq::get(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .set("User-Agent", "graviz-update-check")
    .set("Accept", "application/vnd.github+json")
    .call()
    .ok()?
    .into_json()
    .ok()?;
    parse_release(&body, env!("CARGO_PKG_VERSION"))
}

/// Pulled out of `check_for_update` so the tag/version comparison can be
/// tested without a real network call.
fn parse_release(body: &serde_json::Value, current: &str) -> Option<UpdateInfo> {
    let tag = body.get("tag_name")?.as_str()?;
    let url = body.get("html_url")?.as_str()?;
    let latest = tag.trim_start_matches('v');
    (latest != current).then(|| UpdateInfo { version: latest.to_string(), url: url.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn newer_tag_yields_update_info() {
        let body = json!({ "tag_name": "v0.2.0", "html_url": "https://github.com/x/y/releases/tag/v0.2.0" });
        let info = parse_release(&body, "0.1.0").expect("newer version should be reported");
        assert_eq!(info.version, "0.2.0");
        assert_eq!(info.url, "https://github.com/x/y/releases/tag/v0.2.0");
    }

    #[test]
    fn matching_tag_yields_nothing() {
        let body = json!({ "tag_name": "v0.1.0", "html_url": "https://github.com/x/y/releases/tag/v0.1.0" });
        assert!(parse_release(&body, "0.1.0").is_none());
    }

    #[test]
    fn tag_without_a_leading_v_still_compares() {
        let body = json!({ "tag_name": "0.1.0", "html_url": "https://github.com/x/y/releases/tag/0.1.0" });
        assert!(parse_release(&body, "0.1.0").is_none());
    }

    #[test]
    fn malformed_response_yields_nothing() {
        assert!(parse_release(&json!({}), "0.1.0").is_none());
        assert!(parse_release(&json!({ "tag_name": "v0.2.0" }), "0.1.0").is_none());
    }
}
