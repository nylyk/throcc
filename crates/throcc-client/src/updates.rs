use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

const RELEASES: &str = "https://api.github.com/repos/nylyk/throcc/releases?per_page=100";
const RELEASE_PAGE: &str = "https://github.com/nylyk/throcc/releases/tag/";
const TAG_PREFIX: &str = "client-v";
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// A client release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

impl Release {
    fn tagged(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            url: format!("{RELEASE_PAGE}{TAG_PREFIX}{version}"),
        }
    }

    pub fn sample() -> Self {
        Self::tagged("0.2.0")
    }
}

/// Whether this copy looks for updates itself. A flatpak gets them from flatpak.
pub fn checked_here() -> bool {
    !Path::new("/.flatpak-info").exists()
}

/// Asks GitHub for the newest client release, if it is newer than this build.
pub fn newer_release() -> Result<Option<Release>> {
    let mut roots = rustls_native_certs::load_native_certs();
    if roots.certs.is_empty() {
        anyhow::bail!("no root certificates: {:?}", roots.errors);
    }
    let roots: Vec<_> = roots
        .certs
        .drain(..)
        .map(|cert| Certificate::from_der(&cert).to_owned())
        .collect();
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .root_certs(RootCerts::new_with_certs(&roots))
        .build();
    let agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("throcc/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let body = agent
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()?;
    newer_than(env!("CARGO_PKG_VERSION"), &body)
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

/// The newest published client release in GitHub's release list `json` above `current`.
fn newer_than(current: &str, json: &str) -> Result<Option<Release>> {
    let current = parse(current).context("this build's version")?;
    let releases: Vec<GithubRelease> = serde_json::from_str(json)?;
    let newest = releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = release.tag_name.strip_prefix(TAG_PREFIX)?;
            Some((parse(version)?, version))
        })
        .filter(|(parsed, _)| *parsed > current)
        .max_by_key(|(parsed, _)| *parsed);
    Ok(newest.map(|(_, version)| Release::tagged(version)))
}

/// `major.minor.patch` with nothing else, so pre-releases never parse.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let number = |part: &str| {
        let digits = part.bytes().all(|b| b.is_ascii_digit());
        digits.then(|| part.parse().ok()).flatten()
    };
    let mut parts = version.split('.');
    let parsed = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{prerelease},"name":"x"}}"#)
    }

    #[test]
    fn the_newest_published_client_release_above_this_one_wins() {
        let json = format!(
            "[{},{},{},{},{},{}]",
            release("server-v0.9.0", false, false),
            release("client-v0.3.0", true, false),
            release("client-v0.2.1", false, true),
            release("client-v0.2.0", false, false),
            release("client-v0.10.0-rc.1", false, false),
            release("client-v0.1.2", false, false),
        );
        assert_eq!(
            newer_than("0.1.0", &json).unwrap(),
            Some(Release {
                version: "0.2.0".into(),
                url: "https://github.com/nylyk/throcc/releases/tag/client-v0.2.0".into(),
            })
        );
        assert_eq!(newer_than("0.2.0", &json).unwrap(), None);
        assert_eq!(newer_than("0.10.0", "[]").unwrap(), None);
    }

    #[test]
    #[ignore = "asks GitHub over the network"]
    fn github_answers_the_update_check() {
        newer_release().unwrap();
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(parse("0.10.0") > parse("0.9.9"));
        assert_eq!(parse("1.2.3"), Some((1, 2, 3)));
        for odd in ["1.2", "1.2.3.4", "1.2.3-rc.1", "+1.2.3", "1..3", ""] {
            assert_eq!(parse(odd), None, "{odd}");
        }
    }
}
