use crate::error::AppError;
use crate::extractors::RequireAdmin;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const GITHUB_REPO: &str = "iant4093/TruongPhoto";
const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/iant4093/TruongPhoto/releases/latest";
const GITHUB_RELEASES_PAGE_URL: &str = "https://github.com/iant4093/TruongPhoto/releases";

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheckResponse {
    pub status: &'static str,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_name: Option<String>,
    pub release_notes: Option<String>,
    pub published_at: Option<String>,
    pub release_url: String,
    pub download_url: Option<String>,
    pub asset_name: Option<String>,
    pub repo: &'static str,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    let core = cleaned.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(cur), Some(lat)) => lat > cur,
        _ => {
            let c = current.trim().trim_start_matches(['v', 'V']);
            let l = latest.trim().trim_start_matches(['v', 'V']);
            !l.is_empty() && l != c
        }
    }
}

fn pick_platform_asset<'a>(assets: &'a [GithubAsset], os: &str) -> Option<&'a GithubAsset> {
    let preferred_exts: &[&str] = match os {
        "macos" => &[".dmg", ".app.tar.gz", ".zip"],
        "windows" => &[".msi", "-setup.exe", ".exe", ".zip"],
        _ => &[".appimage", ".deb", ".tar.gz"],
    };

    for ext in preferred_exts {
        if let Some(found) = assets
            .iter()
            .find(|a| a.name.to_ascii_lowercase().ends_with(ext))
        {
            return Some(found);
        }
    }
    assets.first()
}

/// `GET /api/admin/updates/check` — checks GitHub Releases for a newer TruongPhoto release.
pub async fn check_for_updates(
    RequireAdmin(_admin): RequireAdmin,
) -> Result<Json<UpdateCheckResponse>, AppError> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(format!("TruongPhoto/{current_version}"))
        .build()
        .map_err(|e| AppError::internal(format!("update http client: {e}")))?;

    let resp = match client
        .get(GITHUB_LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::debug!(error = %err, "GitHub update check unreachable");
            return Ok(Json(UpdateCheckResponse {
                status: "offline",
                current_version,
                latest_version: None,
                update_available: false,
                release_name: None,
                release_notes: None,
                published_at: None,
                release_url: GITHUB_RELEASES_PAGE_URL.to_string(),
                download_url: None,
                asset_name: None,
                repo: GITHUB_REPO,
                message: Some(
                    "Could not reach GitHub Releases. Check your internet connection and try again."
                        .to_string(),
                ),
            }));
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Json(UpdateCheckResponse {
            status: "no_releases",
            current_version,
            latest_version: None,
            update_available: false,
            release_name: None,
            release_notes: None,
            published_at: None,
            release_url: GITHUB_RELEASES_PAGE_URL.to_string(),
            download_url: None,
            asset_name: None,
            repo: GITHUB_REPO,
            message: Some(
                "No published GitHub releases were found yet. You are running the latest local build."
                    .to_string(),
            ),
        }));
    }

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        return Ok(Json(UpdateCheckResponse {
            status: "offline",
            current_version,
            latest_version: None,
            update_available: false,
            release_name: None,
            release_notes: None,
            published_at: None,
            release_url: GITHUB_RELEASES_PAGE_URL.to_string(),
            download_url: None,
            asset_name: None,
            repo: GITHUB_REPO,
            message: Some(format!("GitHub Releases returned HTTP {code}.")),
        }));
    }

    let release: GithubRelease = match resp.json().await {
        Ok(rel) => rel,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to decode GitHub release payload");
            return Ok(Json(UpdateCheckResponse {
                status: "offline",
                current_version,
                latest_version: None,
                update_available: false,
                release_name: None,
                release_notes: None,
                published_at: None,
                release_url: GITHUB_RELEASES_PAGE_URL.to_string(),
                download_url: None,
                asset_name: None,
                repo: GITHUB_REPO,
                message: Some("Received an unexpected response from GitHub Releases.".to_string()),
            }));
        }
    };

    let latest_clean = release
        .tag_name
        .trim()
        .trim_start_matches(['v', 'V'])
        .to_string();
    let update_available = is_newer_version(&current_version, &latest_clean);
    let matched_asset = pick_platform_asset(&release.assets, std::env::consts::OS);

    Ok(Json(UpdateCheckResponse {
        status: if update_available {
            "update_available"
        } else {
            "up_to_date"
        },
        current_version,
        latest_version: Some(latest_clean),
        update_available,
        release_name: release.name.filter(|s| !s.trim().is_empty()),
        release_notes: release.body.filter(|s| !s.trim().is_empty()),
        published_at: release.published_at,
        release_url: release.html_url,
        download_url: matched_asset.map(|a| a.browser_download_url.clone()),
        asset_name: matched_asset.map(|a| a.name.clone()),
        repo: GITHUB_REPO,
        message: if update_available {
            None
        } else {
            Some("You are running the latest release.".to_string())
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_v_prefix_and_prerelease() {
        assert!(is_newer_version("0.1.0", "v0.2.0"));
        assert!(is_newer_version("0.1.0", "0.1.1"));
        assert!(is_newer_version("0.1.9", "1.0.0"));
        assert!(!is_newer_version("0.2.0", "v0.2.0"));
        assert!(!is_newer_version("0.2.1", "v0.2.0"));
    }

    #[test]
    fn platform_asset_picker_selects_expected_installer() {
        let assets = vec![
            GithubAsset {
                name: "TruongPhoto_0.2.0_x64-setup.exe".into(),
                browser_download_url: "https://example.com/win.exe".into(),
            },
            GithubAsset {
                name: "TruongPhoto_0.2.0_aarch64.dmg".into(),
                browser_download_url: "https://example.com/mac.dmg".into(),
            },
        ];
        assert_eq!(
            pick_platform_asset(&assets, "macos").map(|a| a.name.as_str()),
            Some("TruongPhoto_0.2.0_aarch64.dmg")
        );
        assert_eq!(
            pick_platform_asset(&assets, "windows").map(|a| a.name.as_str()),
            Some("TruongPhoto_0.2.0_x64-setup.exe")
        );
    }
}
