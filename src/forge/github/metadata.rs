//! Release-note attribution from GitHub's GraphQL API: author and merged PR for
//! each released commit, plus which authors contribute for the first time. Costs
//! a few batched queries per run instead of paging the repo's whole history.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use octocrab::Octocrab;
use serde::Deserialize;

use crate::forge::RepoRef;

/// Commits looked up per GraphQL query.
const COMMIT_BATCH: usize = 50;
/// Authors checked for earlier commits per GraphQL query.
const AUTHOR_BATCH: usize = 25;
/// GitHub's page-size cap for a connection.
const MAX_FIRST: usize = 100;

const ATTRIBUTION_FRAGMENT: &str = "fragment attribution on Commit { \
    author { user { id login } } \
    associatedPullRequests(first: 5) { nodes { number title merged labels(first: 20) { nodes { name } } } } }";

/// Who wrote a commit and which merged pull request brought it in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitAttribution {
    pub login: Option<String>,
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestRef {
    pub number: i64,
    pub title: String,
    pub labels: Vec<String>,
}

/// Attribution for a run's released commits, keyed by full SHA, and the logins
/// whose only commits are the ones released in this run.
#[derive(Debug, Default)]
pub struct ReleaseAttribution {
    pub commits: HashMap<String, CommitAttribution>,
    pub first_time: HashSet<String>,
}

#[derive(Deserialize)]
struct Repository<T> {
    repository: T,
}

#[derive(Deserialize)]
struct Connection<T> {
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitNode {
    author: Option<GitActor>,
    associated_pull_requests: Option<Connection<PullRequestNode>>,
}

#[derive(Deserialize)]
struct GitActor {
    user: Option<User>,
}

#[derive(Deserialize)]
struct User {
    id: String,
    login: String,
}

#[derive(Deserialize)]
struct PullRequestNode {
    number: i64,
    title: String,
    merged: bool,
    labels: Option<Connection<Label>>,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct HistoryRoot {
    head: Option<HashMap<String, Connection<OidNode>>>,
}

#[derive(Deserialize)]
struct OidNode {
    oid: String,
}

/// An author of released commits; keyed by GraphQL node id.
struct Author {
    login: String,
    released: usize,
}

/// GraphQL lives at `/api/graphql` on GitHub Enterprise, beside the REST `/api/v3`
/// base, and octocrab joins its `/graphql` route onto the base path.
pub(super) fn graphql_base_uri(api_url: Option<&str>) -> Option<String> {
    api_url.map(|url| url.strip_suffix("/v3").unwrap_or(url).to_string())
}

fn quoted(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

fn commits_query(shas: &[String]) -> String {
    let aliases: String = shas
        .iter()
        .enumerate()
        .map(|(i, sha)| format!(" c{i}: object(oid: {}) {{ ...attribution }}", quoted(sha)))
        .collect();
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{{aliases} }} }} {ATTRIBUTION_FRAGMENT}"
    )
}

/// `pages` pairs each author's node id with how many of their commits to fetch.
fn history_query(head: &str, pages: &[(&str, usize)]) -> String {
    let aliases: String = pages
        .iter()
        .enumerate()
        .map(|(i, (id, first))| {
            format!(
                " u{i}: history(first: {first}, author: {{ id: {} }}) {{ nodes {{ oid }} }}",
                quoted(id)
            )
        })
        .collect();
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ head: object(oid: {}) {{ ... on Commit {{{aliases} }} }} }} }}",
        quoted(head)
    )
}

/// First-time when the author's history holds nothing but released commits and
/// the page came back short; a full page that is all released stays unproven.
fn is_first_time(history: &[String], released: &HashSet<&str>, page: usize) -> bool {
    history.len() < page && history.iter().all(|oid| released.contains(oid.as_str()))
}

fn attribution(node: &CommitNode) -> CommitAttribution {
    let pull_request = node
        .associated_pull_requests
        .iter()
        .flat_map(|prs| prs.nodes.iter().flatten())
        .find(|pr| pr.merged)
        .map(|pr| PullRequestRef {
            number: pr.number,
            title: pr.title.clone(),
            labels: pr
                .labels
                .iter()
                .flat_map(|labels| labels.nodes.iter().flatten())
                .map(|label| label.name.clone())
                .collect(),
        });
    CommitAttribution {
        login: node
            .author
            .as_ref()
            .and_then(|a| a.user.as_ref())
            .map(|u| u.login.clone()),
        pull_request,
    }
}

async fn query<T: serde::de::DeserializeOwned>(
    client: &Octocrab,
    repo: &RepoRef,
    query: String,
) -> Result<T> {
    let payload = serde_json::json!({
        "query": query,
        "variables": { "owner": repo.owner, "name": repo.repo },
    });
    let data: Repository<T> = client.graphql(&payload).await?;
    Ok(data.repository)
}

/// Attribute `shas` (full SHAs, deduplicated) and decide first-time authors against
/// the history of `head`; without a head nobody is marked first-time.
pub(super) async fn release_attribution(
    client: &Octocrab,
    repo: &RepoRef,
    head: Option<&str>,
    shas: &[String],
) -> Result<ReleaseAttribution> {
    let mut result = ReleaseAttribution::default();
    let mut authors: HashMap<String, Author> = HashMap::new();
    for batch in shas.chunks(COMMIT_BATCH) {
        let mut nodes: HashMap<String, Option<CommitNode>> =
            query(client, repo, commits_query(batch)).await?;
        for (i, sha) in batch.iter().enumerate() {
            let Some(node) = nodes.remove(&format!("c{i}")).flatten() else {
                continue;
            };
            if let Some(user) = node.author.as_ref().and_then(|a| a.user.as_ref()) {
                authors
                    .entry(user.id.clone())
                    .or_insert_with(|| Author {
                        login: user.login.clone(),
                        released: 0,
                    })
                    .released += 1;
            }
            result.commits.insert(sha.clone(), attribution(&node));
        }
    }

    let Some(head) = head else {
        return Ok(result);
    };
    let released: HashSet<&str> = shas.iter().map(String::as_str).collect();
    let authors: Vec<(&String, &Author)> = authors.iter().collect();
    for batch in authors.chunks(AUTHOR_BATCH) {
        // One more commit than each author released, so an earlier one would show up.
        let pages: Vec<(&str, usize)> = batch
            .iter()
            .map(|(id, author)| (id.as_str(), (author.released + 1).min(MAX_FIRST)))
            .collect();
        let root: HistoryRoot = query(client, repo, history_query(head, &pages)).await?;
        let mut histories = root.head.unwrap_or_default();
        for (i, ((_, author), (_, page))) in batch.iter().zip(&pages).enumerate() {
            let history: Vec<String> = histories
                .remove(&format!("u{i}"))
                .map(|c| c.nodes.into_iter().flatten().map(|n| n.oid).collect())
                .unwrap_or_default();
            if is_first_time(&history, &released, *page) {
                result.first_time.insert(author.login.clone());
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_base_uri_maps_enterprise_rest_base() {
        assert_eq!(graphql_base_uri(None), None);
        assert_eq!(
            graphql_base_uri(Some("https://ghe.corp/api/v3")).as_deref(),
            Some("https://ghe.corp/api")
        );
    }

    /// A rebase-merged commit is credited through `associatedPullRequests`, skipping unmerged PRs.
    #[test]
    fn attribution_picks_the_merged_pull_request() {
        let node: CommitNode = serde_json::from_value(serde_json::json!({
            "author": { "user": { "id": "U_1", "login": "octocat" } },
            "associatedPullRequests": { "nodes": [
                { "number": 3, "title": "draft", "merged": false, "labels": { "nodes": [] } },
                { "number": 7, "title": "Add a thing", "merged": true,
                  "labels": { "nodes": [{ "name": "enhancement" }] } }
            ] }
        }))
        .unwrap();

        assert_eq!(
            attribution(&node),
            CommitAttribution {
                login: Some("octocat".into()),
                pull_request: Some(PullRequestRef {
                    number: 7,
                    title: "Add a thing".into(),
                    labels: vec!["enhancement".into()],
                }),
            }
        );
    }

    #[test]
    fn first_time_only_when_history_holds_just_released_commits() {
        let released: HashSet<&str> = ["a", "b"].into();
        let history = |oids: &[&str]| oids.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(is_first_time(&history(&["b", "a"]), &released, 3));
        assert!(!is_first_time(&history(&["b", "a", "older"]), &released, 3));
        assert!(!is_first_time(&history(&["a", "b"]), &released, 2));
    }

    /// Runs both real queries against this repository.
    #[test]
    #[ignore = "needs GITHUB_TOKEN and network access"]
    fn live_attribution_against_github() {
        let token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN");
        let repo = RepoRef {
            owner: "BowlingX".into(),
            repo: "super-release".into(),
            host: "github.com".into(),
        };
        let squash_merged = "982504bb2962834cc85696694a39f52a26cc1603";
        let pushed = "28646641a2a90582ce1a5d7c41d4f61ac9f20884";
        let head = "1845b6f65b61f73727cad376993d4b464bad2971";

        let result = crate::forge::github::GitHubForge
            .release_attribution(
                &token,
                None,
                &repo,
                Some(head),
                &[squash_merged.into(), pushed.into()],
            )
            .unwrap();

        let pr = |sha: &str| {
            result.commits[sha]
                .pull_request
                .as_ref()
                .map(|pr| pr.number)
        };
        assert_eq!(pr(squash_merged), Some(52));
        assert_eq!(pr(pushed), None);
        assert_eq!(
            result.commits[squash_merged].login.as_deref(),
            Some("BowlingX")
        );
        assert!(result.first_time.is_empty(), "{:?}", result.first_time);
    }

    #[test]
    fn queries_alias_each_commit_and_author() {
        let commits = commits_query(&["aaa".into(), "bbb".into()]);
        assert!(commits.contains(r#"c0: object(oid: "aaa")"#), "{commits}");
        assert!(commits.contains(r#"c1: object(oid: "bbb")"#), "{commits}");

        let history = history_query("head", &[("U_1", 2)]);
        assert!(
            history.contains(r#"head: object(oid: "head")"#),
            "{history}"
        );
        assert!(
            history.contains(r#"u0: history(first: 2, author: { id: "U_1" })"#),
            "{history}"
        );
    }
}
