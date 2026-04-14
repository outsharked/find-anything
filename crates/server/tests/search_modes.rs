mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::SearchResponse;

// ── exact mode ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_exact_mode_matches_substring() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "exact.txt", "the quick brown fox jumps over the lazy dog");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=quick+brown+fox&mode=exact&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "exact phrase should match");
    assert!(resp.results.iter().any(|r| r.path == "exact.txt"));
}

#[tokio::test]
async fn test_exact_mode_does_not_match_fuzzy_variants() {
    let srv = TestServer::spawn().await;
    // Index a file with "colour" (British spelling)
    let req = make_text_bulk("docs", "spelling.txt", "the colour of the sky is blue");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Exact search for "color" (American spelling) should return no results
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=color&mode=exact&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.total, 0, "exact mode must not match fuzzy variants");
}

// ── regex mode ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_regex_mode_matches_pattern() {
    let srv = TestServer::spawn().await;
    // Use literal words that FTS5 can find as a pre-filter, connected by a regex wildcard.
    // regex_to_fts_terms extracts "fatal" and "encountered" as literal FTS5 terms.
    let req = make_text_bulk("docs", "regex.txt", "fatal error encountered at runtime");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Pattern: "fatal" ... "encountered" with anything in between
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=fatal.%2Bencountered&mode=regex&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "regex mode should match fatal.+encountered pattern");
    assert!(resp.results.iter().any(|r| r.path == "regex.txt"));
}

#[tokio::test]
async fn test_regex_mode_no_match() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "noregex.txt", "ordinary text with no special codes");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Pattern that shouldn't match
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=%5BXYZ%5D%7B5%7D&mode=regex&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.total, 0, "regex mode should return empty for non-matching pattern");
}

// ── file-* modes ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_file_fuzzy_mode_matches_filename() {
    let srv = TestServer::spawn().await;
    // The content is different from the filename — only the filename should match
    let req = make_text_bulk("docs", "invoices/report2024.txt", "unrelated content here");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=report2024&mode=file-fuzzy&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "file-fuzzy should match filename");
    assert!(resp.results.iter().any(|r| r.path.contains("report2024")));
}

#[tokio::test]
async fn test_file_fuzzy_does_not_match_content_only() {
    let srv = TestServer::spawn().await;
    // The filename does not contain the search term, only the content does
    let req = make_text_bulk("docs", "document.txt", "zymurgy fermentation content");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=zymurgy&mode=file-fuzzy&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.total, 0, "file-fuzzy should only match filenames, not content");
}

#[tokio::test]
async fn test_file_exact_matches_exact_filename_fragment() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "data/quarterly_report.txt", "some financial content");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=quarterly_report&mode=file-exact&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "file-exact should match the exact filename fragment");
}

// ── file-regex mode ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_file_regex_matches_filename_pattern() {
    let srv = TestServer::spawn().await;
    srv.post_bulk(&make_text_bulk("docs", "invoices/report_2024_q1.txt", "unrelated content")).await;
    srv.post_bulk(&make_text_bulk("docs", "notes/todo.txt", "unrelated content")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=report_%5Cd%7B4%7D&mode=file-regex&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "file-regex should match filename pattern");
    assert!(resp.results.iter().any(|r| r.path.contains("report_2024")));
    assert!(!resp.results.iter().any(|r| r.path == "notes/todo.txt"), "non-matching file must be excluded");
}

// ── document mode ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_document_mode_groups_by_file() {
    let srv = TestServer::spawn().await;
    // File with the query word on multiple lines — document mode should return one result.
    srv.post_bulk(&make_text_bulk("docs", "multi.txt",
        "alpha keyword line one\nalpha keyword line two\nalpha keyword line three")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=alpha+keyword&mode=document&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "document mode should return at least one result");
    // Must not return more results than files (grouped per file).
    let paths: std::collections::HashSet<&str> = resp.results.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains("multi.txt"), "multi.txt should appear in document results");
}

#[tokio::test]
async fn test_document_mode_multi_keyword_all_lines() {
    let srv = TestServer::spawn().await;
    // Each keyword is on a separate line so they produce distinct FTS candidate rows.
    srv.post_bulk(&make_text_bulk("docs", "keywords.txt",
        "line with alpha here\nsome other content\nline with bravo here")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=alpha+bravo&mode=document&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "document mode should find the file");
    // Document mode now returns one SearchResult per matching line.
    let file_results: Vec<_> = resp.results.iter().filter(|r| r.path == "keywords.txt").collect();
    assert!(file_results.len() >= 2, "should have at least 2 results for keywords.txt (one per keyword line)");
    let all_snippets: Vec<&str> = file_results.iter().map(|r| r.snippet.as_str()).collect();
    assert!(all_snippets.iter().any(|s| s.contains("alpha")), "alpha should appear in some result snippet");
    assert!(all_snippets.iter().any(|s| s.contains("bravo")), "bravo should appear in some result snippet");
}

#[tokio::test]
async fn test_doc_exact_mode_matches_phrase() {
    let srv = TestServer::spawn().await;
    srv.post_bulk(&make_text_bulk("docs", "phrase.txt", "the exact phrase to find here")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=exact+phrase&mode=doc-exact&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "doc-exact should match the phrase");
    assert!(resp.results.iter().any(|r| r.path == "phrase.txt"));
}

#[tokio::test]
async fn test_doc_regex_mode_matches_pattern() {
    let srv = TestServer::spawn().await;
    srv.post_bulk(&make_text_bulk("docs", "log.txt",
        "error occurred at line 42\nno problem here\nanother error at line 99")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=error.*line&mode=doc-regex&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "doc-regex should match cross-line pattern");
    assert!(resp.results.iter().any(|r| r.path == "log.txt"));
}

#[tokio::test]
async fn test_doc_regex_no_match_returns_empty() {
    let srv = TestServer::spawn().await;
    srv.post_bulk(&make_text_bulk("docs", "clean.txt", "nothing suspicious here")).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=ZZZZZUNLIKELY%5Cd%7B8%7D&mode=doc-regex&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.total, 0, "doc-regex with no match should return empty");
}

// ── fuzzy (default) mode ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_fuzzy_mode_is_default() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "fuzzy.txt", "documentation about configuration options");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // No mode param — should default to fuzzy and find results
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=configuration&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "default mode should be fuzzy and find matches");
}
