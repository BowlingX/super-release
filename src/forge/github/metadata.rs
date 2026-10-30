//! Release-note attribution from GitHub's GraphQL API: author and merged PR for
//! each released commit, which authors contribute for the first time, and the
//! released PRs' comments. Costs a few batched queries per run instead of paging
//! the repo's whole history.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use octocrab::Octocrab;
use serde::Deserialize;

use crate::forge::RepoRef;

/// Commits looked up per GraphQL query.
const COMMIT_BATCH: usize = 50;
/// Authors checked for earlier commits per GraphQL query.
const AUTHOR_BATCH: usize = 25;
/// Pull requests (with their comments and commits) looked up per GraphQL query.
const PULL_REQUEST_BATCH: usize = 10;
/// GitHub's page-size cap for a connection.
const MAX_FIRST: usize = 100;

const ATTRIBUTION_FRAGMENT: &str = "fragment attribution on Commit { \
    author { user { id login } } \
    associatedPullRequests(first: 10) { nodes { number title merged mergedAt labels(first: 20) { nodes { name } } } } }";

/// Commits page on: a release PR can carry more than one page.
const PULL_REQUEST_FRAGMENT: &str = "fragment pr on PullRequest { \
    number title url mergedAt \
    commits(first: 100) { pageInfo { hasNextPage endCursor } nodes { commit { oid } } } \
    comments(last: 100) { nodes { databaseId author { login } authorAssociation body url createdAt lastEditedAt } } }";

/// Who wrote a commit and which merged pull requests brought it in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitAttribution {
    pub login: Option<String>,
    /// Merged PRs containing the commit, earliest merge first.
    pub pull_requests: Vec<PullRequestRef>,
}

impl CommitAttribution {
    /// The PR that introduced the commit: the earliest merged one.
    pub fn pull_request(&self) -> Option<&PullRequestRef> {
        self.pull_requests.first()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestRef {
    pub number: i64,
    pub title: String,
    pub labels: Vec<String>,
    /// RFC 3339 merge time (UTC, so it orders as a string).
    pub merged_at: Option<String>,
}

/// A pull-request comment, as read for release-notes directives.
#[derive(Debug, Clone, PartialEq)]
pub struct PrComment {
    pub id: u64,
    /// `None` for deleted ("ghost") accounts.
    pub author: Option<String>,
    /// The author's relation to the repo, e.g. `MEMBER` or `CONTRIBUTOR`.
    pub association: String,
    pub body: String,
    pub url: String,
    /// RFC 3339 time of the last edit, else of creation.
    pub written_at: String,
}

/// A pull request's comments (oldest first) and commits (full SHAs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PullRequestInfo {
    pub number: i64,
    pub title: String,
    pub url: String,
    /// RFC 3339 merge time (UTC); `None` while open.
    pub merged_at: Option<String>,
    pub commits: HashSet<String>,
    pub comments: Vec<PrComment>,
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
#[serde(rename_all = "camelCase")]
struct PullRequestNode {
    number: i64,
    title: String,
    merged: bool,
    merged_at: Option<String>,
    labels: Option<Connection<Label>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestDetails {
    number: i64,
    title: String,
    url: String,
    merged_at: Option<String>,
    commits: CommitPage,
    comments: Connection<CommentNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitPage {
    page_info: PageInfo,
    nodes: Vec<Option<PullRequestCommit>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct PullRequestCommit {
    commit: OidNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentNode {
    database_id: Option<u64>,
    author: Option<Login>,
    author_association: String,
    body: String,
    url: String,
    created_at: String,
    last_edited_at: Option<String>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct CommitsOnly {
    commits: CommitPage,
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

fn pull_requests_query(numbers: &[i64]) -> String {
    let aliases: String = numbers
        .iter()
        .enumerate()
        .map(|(i, n)| format!(" p{i}: pullRequest(number: {n}) {{ ...pr }}"))
        .collect();
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{{aliases} }} }} {PULL_REQUEST_FRAGMENT}"
    )
}

fn pull_request_commits_query(number: i64, after: &str) -> String {
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ \
         pr: pullRequest(number: {number}) {{ commits(first: {MAX_FIRST}, after: {}) {{ \
         pageInfo {{ hasNextPage endCursor }} nodes {{ commit {{ oid }} }} }} }} }} }}",
        quoted(after)
    )
}

/// First-time when the author's history holds nothing but released commits and
/// the page came back short; a full page that is all released stays unproven.
fn is_first_time(history: &[String], released: &HashSet<&str>, page: usize) -> bool {
    history.len() < page && history.iter().all(|oid| released.contains(oid.as_str()))
}

fn attribution(node: &CommitNode) -> CommitAttribution {
    let mut pull_requests: Vec<PullRequestRef> = node
        .associated_pull_requests
        .iter()
        .flat_map(|prs| prs.nodes.iter().flatten())
        .filter(|pr| pr.merged)
        .map(|pr| PullRequestRef {
            number: pr.number,
            title: pr.title.clone(),
            labels: pr
                .labels
                .iter()
                .flat_map(|labels| labels.nodes.iter().flatten())
                .map(|label| label.name.clone())
                .collect(),
            merged_at: pr.merged_at.clone(),
        })
        .collect();
    pull_requests.sort_by(|a, b| merge_order(a.merged_at.as_deref(), b.merged_at.as_deref()));
    CommitAttribution {
        login: node
            .author
            .as_ref()
            .and_then(|a| a.user.as_ref())
            .map(|u| u.login.clone()),
        pull_requests,
    }
}

/// Earliest merge first; unmerged (no time) last.
pub fn merge_order(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn comment(node: CommentNode) -> Option<PrComment> {
    Some(PrComment {
        id: node.database_id?,
        author: node.author.map(|a| a.login),
        association: node.author_association,
        body: node.body,
        url: node.url,
        written_at: node.last_edited_at.unwrap_or(node.created_at),
    })
}

fn oids(page: &CommitPage) -> impl Iterator<Item = String> + '_ {
    page.nodes.iter().flatten().map(|c| c.commit.oid.clone())
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

/// Merged PRs containing `head`, e.g. the release PR whose merge triggered the run.
pub(super) async fn head_pull_requests(
    client: &Octocrab,
    repo: &RepoRef,
    head: &str,
) -> Result<Vec<i64>> {
    let mut nodes: HashMap<String, Option<CommitNode>> =
        query(client, repo, commits_query(&[head.to_string()])).await?;
    Ok(nodes
        .remove("c0")
        .flatten()
        .map(|node| {
            attribution(&node)
                .pull_requests
                .into_iter()
                .map(|pr| pr.number)
                .collect()
        })
        .unwrap_or_default())
}

/// Page through a PR's commits past the first page, into `commits`.
async fn rest_of_commits(
    client: &Octocrab,
    repo: &RepoRef,
    number: i64,
    mut page_info: PageInfo,
    commits: &mut HashSet<String>,
) -> Result<()> {
    while page_info.has_next_page
        && let Some(cursor) = page_info.end_cursor.as_deref()
    {
        let mut page: HashMap<String, Option<CommitsOnly>> =
            query(client, repo, pull_request_commits_query(number, cursor)).await?;
        let Some(next) = page.remove("pr").flatten() else {
            break;
        };
        commits.extend(oids(&next.commits));
        page_info = next.commits.page_info;
    }
    Ok(())
}

/// Comments and commits of each PR in `numbers`; PRs that don't exist are skipped.
pub(super) async fn pull_requests(
    client: &Octocrab,
    repo: &RepoRef,
    numbers: &[i64],
) -> Result<Vec<PullRequestInfo>> {
    let mut result = Vec::with_capacity(numbers.len());
    for batch in numbers.chunks(PULL_REQUEST_BATCH) {
        let mut nodes: HashMap<String, Option<PullRequestDetails>> =
            query(client, repo, pull_requests_query(batch)).await?;
        for i in 0..batch.len() {
            let Some(pr) = nodes.remove(&format!("p{i}")).flatten() else {
                continue;
            };
            let mut commits: HashSet<String> = oids(&pr.commits).collect();
            rest_of_commits(client, repo, pr.number, pr.commits.page_info, &mut commits).await?;
            result.push(PullRequestInfo {
                number: pr.number,
                title: pr.title,
                url: pr.url,
                merged_at: pr.merged_at,
                commits,
                comments: pr
                    .comments
                    .nodes
                    .into_iter()
                    .flatten()
                    .filter_map(comment)
                    .collect(),
            });
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
                { "number": 3, "title": "draft", "merged": false, "mergedAt": null,
                  "labels": { "nodes": [] } },
                { "number": 7, "title": "Add a thing", "merged": true,
                  "mergedAt": "2026-01-02T00:00:00Z",
                  "labels": { "nodes": [{ "name": "enhancement" }] } }
            ] }
        }))
        .unwrap();

        assert_eq!(
            attribution(&node),
            CommitAttribution {
                login: Some("octocat".into()),
                pull_requests: vec![PullRequestRef {
                    number: 7,
                    title: "Add a thing".into(),
                    labels: vec!["enhancement".into()],
                    merged_at: Some("2026-01-02T00:00:00Z".into()),
                }],
            }
        );
    }

    /// The feature PR is credited, not the later release PR that carried the commit.
    #[test]
    fn attribution_credits_the_earliest_merged_pull_request() {
        let node: CommitNode = serde_json::from_value(serde_json::json!({
            "author": null,
            "associatedPullRequests": { "nodes": [
                { "number": 20, "title": "Release", "merged": true,
                  "mergedAt": "2026-02-01T00:00:00Z", "labels": null },
                { "number": 14, "title": "Plugin API", "merged": true,
                  "mergedAt": "2026-01-15T00:00:00Z", "labels": null }
            ] }
        }))
        .unwrap();

        let attribution = attribution(&node);
        let numbers: Vec<i64> = attribution.pull_requests.iter().map(|p| p.number).collect();
        assert_eq!(numbers, vec![14, 20]);
        assert_eq!(attribution.pull_request().map(|p| p.number), Some(14));
    }

    #[test]
    fn pull_request_details_deserialize_comments_and_commits() {
        let pr: PullRequestDetails = serde_json::from_value(serde_json::json!({
            "number": 14,
            "title": "Plugin API",
            "url": "https://github.com/o/r/pull/14",
            "mergedAt": null,
            "commits": {
                "pageInfo": { "hasNextPage": true, "endCursor": "abc" },
                "nodes": [{ "commit": { "oid": "aaa" } }]
            },
            "comments": { "nodes": [
                { "databaseId": 1, "author": { "login": "bob" }, "authorAssociation": "MEMBER",
                  "body": "@super-release release-notes\r\nhi", "url": "https://github.com/o/r/pull/14#c1",
                  "createdAt": "2026-01-01T00:00:00Z", "lastEditedAt": "2026-01-03T00:00:00Z" },
                { "databaseId": 2, "author": null, "authorAssociation": "NONE",
                  "body": "x", "url": "u", "createdAt": "2026-01-02T00:00:00Z", "lastEditedAt": null }
            ] }
        }))
        .unwrap();

        assert!(pr.commits.page_info.has_next_page);
        assert_eq!(oids(&pr.commits).collect::<Vec<_>>(), vec!["aaa"]);
        let comments: Vec<PrComment> = pr
            .comments
            .nodes
            .into_iter()
            .flatten()
            .filter_map(comment)
            .collect();
        assert_eq!(comments[0].author.as_deref(), Some("bob"));
        assert_eq!(comments[0].association, "MEMBER");
        assert_eq!(comments[1].author, None, "ghost author");
        assert_eq!(
            comments[0].written_at, "2026-01-03T00:00:00Z",
            "an edit counts"
        );
        assert_eq!(comments[1].written_at, "2026-01-02T00:00:00Z");
    }

    #[test]
    fn pull_request_queries_alias_each_pr_and_page_commits() {
        let prs = pull_requests_query(&[14, 20]);
        assert!(
            prs.contains("p0: pullRequest(number: 14) { ...pr }"),
            "{prs}"
        );
        assert!(
            prs.contains("p1: pullRequest(number: 20) { ...pr }"),
            "{prs}"
        );
        assert!(prs.contains("fragment pr on PullRequest"), "{prs}");

        let page = pull_request_commits_query(20, "cur");
        assert!(
            page.contains(r#"commits(first: 100, after: "cur")"#),
            "{page}"
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

        let pr = |sha: &str| result.commits[sha].pull_request().map(|pr| pr.number);
        assert_eq!(pr(squash_merged), Some(52));
        assert_eq!(pr(pushed), None);
        assert_eq!(
            result.commits[squash_merged].login.as_deref(),
            Some("BowlingX")
        );
        assert!(result.first_time.is_empty(), "{:?}", result.first_time);
    }

    /// Runs the PR query (comments plus commit lists) against this repository.
    #[test]
    #[ignore = "needs GITHUB_TOKEN and network access"]
    fn live_pull_requests_against_github() {
        let token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN");
        let repo = RepoRef {
            owner: "BowlingX".into(),
            repo: "super-release".into(),
            host: "github.com".into(),
        };
        let squash_merged = "982504bb2962834cc85696694a39f52a26cc1603";
        let attribution = crate::forge::github::GitHubForge
            .release_attribution(&token, None, &repo, None, &[squash_merged.into()])
            .unwrap();

        let prs = crate::forge::github::GitHubForge
            .release_pull_requests(&token, None, &repo, &attribution, None, None)
            .unwrap();

        let pr = prs.iter().find(|p| p.number == 52).expect("PR #52");
        assert!(pr.merged_at.is_some());
        assert!(!pr.commits.is_empty());
        assert!(pr.url.ends_with("/pull/52"), "{}", pr.url);
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
