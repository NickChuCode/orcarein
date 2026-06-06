//! Minimal GitHub REST client — just enough to read an issue for the
//! `orcarein issue <n>` self-bootstrap loop.
//!
//! Deliberately thin and dependency-free beyond `reqwest` (already a
//! dependency): no `gh` CLI, no octocrab. Reading a **public** repo's issue
//! needs no token; pass `GITHUB_TOKEN` for private repos or higher rate limits.
//! Opening a PR is intentionally NOT here — E1 stops at the diff for the human
//! to review and push.

use serde::Deserialize;

/// A GitHub issue, reduced to what the agent needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// Errors talking to GitHub.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("GitHub API returned {status}: {body}")]
    Status { status: u16, body: String },

    #[error("could not parse owner/repo from remote URL: {0}")]
    BadRemote(String),
}

/// The wire shape of an issue (only the fields we use). `body` is nullable.
#[derive(Deserialize)]
struct IssueResponse {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
}

/// Fetches issue `number` from `owner/repo`. `token` is optional for public
/// repos.
pub async fn fetch_issue(
    owner: &str,
    repo: &str,
    number: u64,
    token: Option<&str>,
) -> Result<Issue, GithubError> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
    let client = reqwest::Client::new();
    let mut req = client
        .get(&url)
        // GitHub rejects requests without a User-Agent.
        .header("User-Agent", "orcarein")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GithubError::Status {
            status: status.as_u16(),
            body: truncate(&body, 200),
        });
    }

    let ir: IssueResponse = resp.json().await?;
    Ok(Issue {
        number: ir.number,
        title: ir.title,
        body: ir.body.unwrap_or_default(),
    })
}

/// Parses `(owner, repo)` from a GitHub remote URL — https, ssh (`git@`), or
/// `ssh://`, with or without a trailing `.git`.
pub fn parse_owner_repo(remote_url: &str) -> Result<(String, String), GithubError> {
    let s = remote_url.trim();
    let s = s.strip_suffix(".git").unwrap_or(s);

    // Everything after the host, regardless of scheme/separator.
    let after_host = match s.find("github.com") {
        Some(idx) => s[idx + "github.com".len()..].trim_start_matches([':', '/']),
        None => return Err(GithubError::BadRemote(remote_url.to_owned())),
    };

    let mut parts = after_host.split('/').filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some(owner), Some(repo)) => Ok((owner.to_owned(), repo.to_owned())),
        _ => Err(GithubError::BadRemote(remote_url.to_owned())),
    }
}

/// Truncates a string to `max` chars for error messages.
fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() > max {
        s.chars().take(max).collect::<String>() + "…"
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_remote() {
        let (o, r) = parse_owner_repo("https://github.com/NickChuCode/orcarein.git").unwrap();
        assert_eq!((o.as_str(), r.as_str()), ("NickChuCode", "orcarein"));
    }

    #[test]
    fn parses_https_without_git_suffix() {
        let (o, r) = parse_owner_repo("https://github.com/owner/repo").unwrap();
        assert_eq!((o.as_str(), r.as_str()), ("owner", "repo"));
    }

    #[test]
    fn parses_ssh_remote() {
        let (o, r) = parse_owner_repo("git@github.com:NickChuCode/orcarein.git").unwrap();
        assert_eq!((o.as_str(), r.as_str()), ("NickChuCode", "orcarein"));
    }

    #[test]
    fn parses_ssh_scheme_remote() {
        let (o, r) = parse_owner_repo("ssh://git@github.com/owner/repo.git").unwrap();
        assert_eq!((o.as_str(), r.as_str()), ("owner", "repo"));
    }

    #[test]
    fn rejects_non_github_remote() {
        assert!(matches!(
            parse_owner_repo("https://gitlab.com/owner/repo.git"),
            Err(GithubError::BadRemote(_))
        ));
        assert!(parse_owner_repo("https://github.com/owner").is_err());
    }

    #[test]
    fn issue_response_tolerates_null_body() {
        let json = r#"{"number": 7, "title": "Bug", "body": null}"#;
        let ir: IssueResponse = serde_json::from_str(json).unwrap();
        let issue = Issue {
            number: ir.number,
            title: ir.title,
            body: ir.body.unwrap_or_default(),
        };
        assert_eq!(issue.number, 7);
        assert_eq!(issue.title, "Bug");
        assert_eq!(issue.body, "");
    }
}
