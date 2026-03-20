use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_walkdir::WalkDir;
use futures_util::{Stream, TryStreamExt, future, stream};
use tokio::fs;
use tokio::io::AsyncReadExt;

/// Resolves CLI inputs into a merged stream of recursively found OpenAPI spec paths.
///
/// Downloaded specs are always stored in `download_dir` under a stable filename
/// derived from the URL.  If the file already exists and `force_download` is
/// `false`, the existing file is used without re-downloading.
pub fn resolve_inputs(
    inputs: Vec<String>,
    download_dir: PathBuf,
    force_download: bool,
) -> impl Stream<Item = Result<String>> + Send + 'static {
    let streams_of_spec_paths = inputs.into_iter().map(move |input| {
        let download_dir = download_dir.clone();
        if input.starts_with("http://") || input.starts_with("https://") {
            // input is a URL — resolve it asynchronously
            Box::pin(stream::once(async move {
                resolve_url(input, download_dir, force_download).await
            })) as std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>
        } else {
            let (is_file, is_dir) = {
                let p = Path::new(&input);
                (p.is_file(), p.is_dir())
            };
            if is_file {
                // input is a spec file path
                Box::pin(stream::once(future::ready(Ok(input))))
                    as std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>
            } else if is_dir {
                // input is a directory with potential spec files
                Box::pin(collect_openapi_files(input))
                    as std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>
            } else {
                Box::pin(stream::once(future::ready(Err(anyhow!(
                    "'{}' not found — make sure the path is correct",
                    input
                )))))
                    as std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>
            }
        }
    });
    stream::select_all(streams_of_spec_paths)
}

// ─── URL resolution ───────────────────────────────────────────────────────────

/// Candidate spec paths probed in order when a bare base URL is given.
const SPEC_CANDIDATES: &[&str] = &[
    "/openapi.yaml",
    "/openapi.json",
    "/openapi.yml",
    "/swagger.json",
    "/swagger.yaml",
    "/api-docs",
    "/api/openapi.yaml",
    "/api/openapi.json",
];

/// Given an HTTP(S) URL, return a local path to the spec.
///
/// The spec is always stored in `download_dir` under a stable filename derived
/// from the URL.  If the file already exists and `force_download` is `false`,
/// it is returned immediately without downloading.
///
/// Resolution order:
/// 1. If the URL ends with a known spec extension, try it directly.
/// 2. Try appending each candidate suffix to the given URL prefix.
/// 3. Fall back to probing candidates against the origin root.
async fn resolve_url(url: String, download_dir: PathBuf, force_download: bool) -> Result<String> {
    let path = download_dir.join(url_cache_name(&url));

    if path.exists() && !force_download {
        return path
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("download path is not valid UTF-8"));
    }

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow!(e).context("failed to build HTTP client"))?;

    let body = if looks_like_direct_spec_url(&url) {
        // The URL already points at a specific file — try it directly first.
        match try_fetch(&client, &url).await {
            Some(b) => b,
            None => probe_candidates(&client, &url).await?,
        }
    } else {
        probe_candidates(&client, &url).await?
    };

    write_spec(body, &path).await
}

/// Derive a stable, filesystem-safe filename for a URL.
///
/// Uses the URL's own path basename when it looks like a spec file, otherwise
/// falls back to a hash of the full URL so collisions are impossible.
fn url_cache_name(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Strip query string for the basename check.
    let path = url.split('?').next().unwrap_or(url);
    let lower = path.to_lowercase();

    if lower.ends_with(".yaml") || lower.ends_with(".yml") || lower.ends_with(".json") {
        // Use the last path segment as-is, prefixed to avoid collisions across hosts.
        let basename = path.rsplit('/').next().unwrap_or("spec");
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        format!("apin-{:x}-{}", h.finish(), basename)
    } else {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        format!("apin-{:x}.yaml", h.finish())
    }
}

/// Returns `true` if the URL path ends with a recognised spec file extension.
fn looks_like_direct_spec_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    let lower = path.to_lowercase();
    lower.ends_with(".yaml") || lower.ends_with(".yml") || lower.ends_with(".json")
}

/// Try fetching `url`; return the body text if the response is 2xx and the
/// body looks like an OpenAPI document, otherwise `None`.
async fn try_fetch(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    if is_openapi_content(&text) {
        Some(text)
    } else {
        None
    }
}

/// Probe `SPEC_CANDIDATES` against the given URL.
///
/// Two prefix levels are tried in order:
/// 1. The URL itself (trailing slash normalised), e.g. `https://host/api/v3`
/// 2. The origin root, e.g. `https://host`
///    (skipped if it is the same as the URL prefix)
async fn probe_candidates(client: &reqwest::Client, url: &str) -> Result<String> {
    let url_prefix = url.trim_end_matches('/').to_string();
    let origin = base_url(url);

    // Build the ordered list of prefixes to try, deduplicating if the URL
    // has no path component (i.e. it is already the origin).
    let prefixes: Vec<&str> = if url_prefix == origin {
        vec![&url_prefix]
    } else {
        vec![&url_prefix, &origin]
    };

    for prefix in &prefixes {
        for candidate in SPEC_CANDIDATES {
            let probe = format!("{}{}", prefix, candidate);
            if let Some(body) = try_fetch(client, &probe).await {
                return Ok(body);
            }
        }
    }

    Err(anyhow!(
        "could not find an OpenAPI spec at '{}' — tried candidates under: {}",
        url,
        prefixes.join(", ")
    ))
}

/// Returns the scheme + host (+ optional non-standard port) of `url`,
/// i.e. everything up to and including the authority, with no trailing slash.
///
/// Example: `https://api.example.com/v2/stuff` → `https://api.example.com`
fn base_url(url: &str) -> String {
    // Find the end of "scheme://"
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    // Authority ends at the first '/' after the scheme
    let authority_end = url[after_scheme..]
        .find('/')
        .map(|i| after_scheme + i)
        .unwrap_or(url.len());
    url[..authority_end].to_string()
}

/// Returns `true` if a single line declares an OpenAPI or Swagger document root key.
fn is_openapi_line(line: &str) -> bool {
    let l = line.trim_start();
    // YAML:  openapi: / swagger:
    // JSON:  "openapi": / "swagger":
    l.starts_with("openapi:")
        || l.starts_with("swagger:")
        || l.starts_with("\"openapi\":")
        || l.starts_with("\"swagger\":")
}

/// Returns `true` if the content looks like an OpenAPI document.
fn is_openapi_content(text: &str) -> bool {
    text.lines().take(30).any(is_openapi_line)
}

/// Write `body` to `path`, creating parent directories as needed.
async fn write_spec(body: String, path: &PathBuf) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| {
            anyhow!(e).context(format!("failed to create directory '{}'", parent.display()))
        })?;
    }

    fs::write(path, body.as_bytes())
        .await
        .map_err(|e| anyhow!(e).context("failed to write spec file"))?;

    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("spec path is not valid UTF-8"))
}

// ─── Directory scanning ───────────────────────────────────────────────────────

/// Recursively yields all OpenAPI spec files as a stream of paths.
fn collect_openapi_files(root: String) -> impl Stream<Item = Result<String>> + Send + 'static {
    WalkDir::new(&root)
        .map_err(|e| anyhow!(e))
        .try_filter_map(|entry| async move {
            let path = entry.path();
            if !is_supported_format(&path) {
                return Ok(None);
            }
            has_openapi_field(path).await
        })
}

fn is_supported_format(p: &std::path::Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_lowercase),
        Some(s) if s == "yaml" || s == "yml" || s == "json"
    )
}

/// Returns the path as a `String` if the file's first 4 KB contains an
/// `openapi:` / `swagger:` key (YAML) or `"openapi":` / `"swagger":` key (JSON),
/// or `None` otherwise.
async fn has_openapi_field(path: PathBuf) -> Result<Option<String>> {
    let path_str = path.to_str().map(str::to_string);
    let mut reader = fs::File::open(&path)
        .await
        .map_err(|e| {
            anyhow!(e).context(format!(
                "cannot open '{}'",
                path_str.as_deref().unwrap_or_default()
            ))
        })?
        .take(4096);
    let mut buf = Vec::with_capacity(4096);
    reader.read_to_end(&mut buf).await.map_err(|e| anyhow!(e))?;
    let head = std::str::from_utf8(&buf).unwrap_or("");
    Ok(is_openapi_content(head).then_some(path_str).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_collect_openapi_files() {
        let tmp = std::env::temp_dir();
        let dir = tmp.join("apin_test_dir");
        let _ = fs::remove_dir_all(&dir).await; // Clean up from previous test runs
        fs::create_dir(&dir).await.unwrap();

        let valid_spec = dir.join("valid.yaml");
        let invalid_spec = dir.join("invalid.yaml");
        let non_yaml = dir.join("not_a_spec.txt");

        fs::write(&valid_spec, "openapi: 3.0.0").await.unwrap();
        fs::write(&invalid_spec, "not an openapi file")
            .await
            .unwrap();
        fs::write(&non_yaml, "openapi: 3.0.0").await.unwrap();

        let mut paths = collect_openapi_files(dir.to_string_lossy().to_string())
            .try_collect::<Vec<String>>()
            .await
            .unwrap();

        paths.sort();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], valid_spec.to_str().unwrap());
    }

    #[tokio::test]
    async fn test_collect_all_fixtures() {
        let mut actual_result = collect_openapi_files("tests/fixtures".to_string())
            .try_collect::<Vec<String>>()
            .await
            .unwrap();
        actual_result.sort();

        assert_eq!(
            actual_result,
            vec![
                "tests/fixtures/openapi30_fixture.yaml",
                "tests/fixtures/openapi31_fixture.yaml",
                "tests/fixtures/openapi32_fixture.yaml",
                "tests/fixtures/swagger20_fixture.yaml",
            ]
        )
    }

    #[test]
    fn test_is_supported_format() {
        assert!(is_supported_format(&PathBuf::from("spec.yaml")));
        assert!(is_supported_format(&PathBuf::from("spec.yml")));
        assert!(is_supported_format(&PathBuf::from("SPEC.YAML")));
        assert!(is_supported_format(&PathBuf::from("spec.json")));
        assert!(!is_supported_format(&PathBuf::from("spec.txt")));
    }

    #[tokio::test]
    async fn test_has_openapi_field() {
        let tmp = std::env::temp_dir();
        let file_with_field = tmp.join("with_openapi.yaml");
        let file_without_field = tmp.join("without_openapi.yaml");

        fs::write(&file_with_field, "openapi: 3.0.0").await.unwrap();
        fs::write(&file_without_field, "not an openapi file")
            .await
            .unwrap();

        let expected_result = file_with_field.to_str().map(str::to_string);
        assert_eq!(
            has_openapi_field(file_with_field).await.unwrap(),
            expected_result
        );
        assert_eq!(has_openapi_field(file_without_field).await.unwrap(), None);
    }
}
