use nucleo_matcher::{
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

pub struct FuzzyScorer {
    matcher: Matcher,
    pattern: Pattern,
}

impl FuzzyScorer {
    pub fn new(query: &str, case_sensitive: bool) -> Self {
        let matcher = Matcher::new(Config::DEFAULT);
        let case = if case_sensitive { CaseMatching::Respect } else { CaseMatching::Ignore };
        let pattern = Pattern::new(
            query,
            case,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        Self { matcher, pattern }
    }

    /// Returns Some(score) if `haystack` matches, None otherwise.
    pub fn score(&mut self, haystack: &str) -> Option<u32> {
        let mut buf = Vec::new();
        let s = Utf32Str::new(haystack, &mut buf);
        self.pattern.score(s, &mut self.matcher)
    }
}
