use crate::note::{Note, yaml_display};

#[derive(Debug, Default, Clone)]
pub struct Query {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
    /// The words as typed, so import_loose_faces can match whole as well as split into terms.
    pub words: Vec<String>,
    /// The unquoted query as one string, when it has several terms.
    pub whole: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub note: Note,
    pub score: f64,
    pub snippet: String,
    /// How many query terms the note matched.
    pub matched: usize,
    /// The query terms it matched, in query order.
    pub matched_terms: Vec<String>,
    /// The query names this very note, by id, id prefix or slug.
    pub own: bool,
    /// How many query words it holds literally.
    pub exact: usize,
    /// It holds the whole query literally.
    pub exact_whole: bool,
    /// It holds a word of several terms literally, like an identifier or an error code with a dash.
    pub exact_identifier: bool,
    /// Set by callers that know the whole vault: superseded or expired.
    pub lapsed: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    /// Matches before any limit was applied.
    pub total: usize,
    /// True when no note matched every term and results are partial matches.
    pub fell_back_to_or: bool,
    /// Words in the query, to tell partial matches apart.
    pub terms: usize,
}

pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

pub fn parse_query(q: &str) -> Query {
    let mut query = Query::default();
    let mut rest = String::new();
    let mut in_quote = false;
    let mut cur = String::new();
    for c in q.chars() {
        if c == '"' {
            if in_quote {
                let p = cur.trim().to_lowercase();
                if !p.is_empty() {
                    query.phrases.push(p);
                }
                cur.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            cur.push(c);
        } else {
            rest.push(c);
        }
    }
    if in_quote {
        rest.push(' ');
        rest.push_str(&cur);
    }
    for t in tokenize(&rest) {
        if !query.terms.contains(&t) {
            query.terms.push(t);
        }
    }
    let rest = squeeze(&rest.to_lowercase());
    for w in rest.split(' ').map(trim_edges).filter(|w| !w.is_empty()) {
        if !query.words.iter().any(|x| x == w) {
            query.words.push(w.to_string());
        }
    }
    if query.terms.len() > 1 {
        query.whole = Some(trim_edges(&rest).to_string());
    }
    query
}

fn in_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c| !in_word(c))
}

fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `needle` occurs in `hay` with no word character touching it, so faces does not match import_loose_faces.
fn holds(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, m)| !hay[..i].chars().next_back().is_some_and(in_word) && !hay[i + m.len()..].chars().next().is_some_and(in_word))
}

struct Prepared {
    title: Vec<String>,
    slug: Vec<String>,
    tags: Vec<String>,
    refs: Vec<String>,
    fields: Vec<String>,
    body: Vec<String>,
    title_lc: String,
    body_lc: String,
    /// Everything searchable, lower case with white space squeezed, to look for literal words in.
    text: String,
}

fn prepare(n: &Note) -> Prepared {
    let mut fields = vec![];
    let mut text = format!("{} {} {} {}", n.id, n.slug().unwrap_or_default(), n.title, n.body);
    for t in n.tags.iter().chain(n.refs().iter()) {
        text.push(' ');
        text.push_str(t);
    }
    for (k, v) in &n.extra {
        if k.as_str().is_some_and(|k| k == crate::session::SESSION_KEY || k == crate::session::LAST_SESSION_KEY || k == crate::slug::SLUG_KEY || k == crate::note::SAID_ON_KEY) {
            continue;
        }
        fields.extend(tokenize(&yaml_display(v)));
        text.push(' ');
        text.push_str(&yaml_display(v));
    }
    let mut tags = vec![];
    for t in &n.tags {
        tags.extend(tokenize(t));
    }
    Prepared {
        title: tokenize(&n.title),
        slug: n.slug().map(|s| tokenize(&s)).unwrap_or_default(),
        tags,
        refs: n.refs().iter().flat_map(|r| tokenize(r)).collect(),
        fields,
        body: tokenize(&n.body),
        title_lc: n.title.to_lowercase(),
        body_lc: n.body.to_lowercase(),
        text: squeeze(&text.to_lowercase()),
    }
}

/// Exact token hit counts fully, a prefix-only hit counts half.
fn token_weight(tokens: &[String], term: &str) -> f64 {
    if tokens.iter().any(|t| t == term) {
        1.0
    } else if tokens.iter().any(|t| t.starts_with(term)) {
        0.5
    } else {
        0.0
    }
}

fn term_score(n: &Note, p: &Prepared, term: &str) -> f64 {
    let mut s = 0.0;
    s += 6.0 * token_weight(&p.title, term);
    s += 6.0 * token_weight(&p.slug, term);
    s += 4.0 * token_weight(&p.tags, term);
    s += 4.0 * token_weight(&p.refs, term);
    if term.len() >= crate::id::MIN_PREFIX && n.id.starts_with(term) {
        s += 10.0;
    }
    s += 2.0 * token_weight(&p.fields, term);
    let mut body: f64 = 0.0;
    for t in &p.body {
        if t == term {
            body += 1.0;
        } else if t.starts_with(term) {
            body += 0.5;
        }
        if body >= 3.0 {
            break;
        }
    }
    s + body.min(3.0)
}

fn snippet(n: &Note, q: &Query) -> String {
    let mut best: Option<(usize, &str)> = None;
    for line in n.body.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let lc = l.to_lowercase();
        let toks = tokenize(l);
        let mut hits = q.phrases.iter().filter(|p| lc.contains(p.as_str())).count() * 2;
        hits += q.terms.iter().filter(|t| toks.iter().any(|x| x.starts_with(t.as_str()))).count();
        hits += q.words.iter().filter(|w| holds(&lc, w)).count() * (q.terms.len() + 1);
        if hits > 0 && best.is_none_or(|(b, _)| hits > b) {
            best = Some((hits, l));
        }
    }
    best.map(|(_, l)| crate::util::truncate_chars(l, 160)).unwrap_or_default()
}

/// Durable knowledge first, long session logs last: they match many words without being about any of them.
fn kind_weight(kind: &str) -> f64 {
    match kind {
        "decision" | "gotcha" | "feedback" | "user" => 1.15,
        "handoff" | "chat-summary" | "workflow" => 0.7,
        _ => 1.0,
    }
}

/// Up to a fifth more for a note written today, half of that after a month.
fn recency(n: &Note) -> f64 {
    let days = (crate::util::now() - n.updated).num_days().max(0) as f64;
    1.0 + 0.2 * 0.5f64.powf(days / 30.0)
}

/// Scores notes against a query. A note the query names comes first, then notes holding the query or its words literally come first, then notes matching more
/// terms, then by score weighted by kind and recency. Partial matches are kept only within a few terms of the best
/// match, so a long query never lists everything that shares one word with it, a literal identifier always stays.
pub fn search(notes: Vec<Note>, q: &str) -> SearchResult {
    let query = parse_query(q);
    if query.terms.is_empty() && query.phrases.is_empty() {
        return SearchResult { hits: vec![], total: 0, fell_back_to_or: false, terms: 0 };
    }
    let mut hits = vec![];
    for n in notes {
        let p = prepare(&n);
        if !query.phrases.iter().all(|ph| p.title_lc.contains(ph.as_str()) || p.body_lc.contains(ph.as_str())) {
            continue;
        }
        let mut score = 0.0;
        for ph in &query.phrases {
            if p.title_lc.contains(ph.as_str()) {
                score += 6.0;
            }
            if p.body_lc.contains(ph.as_str()) {
                score += 2.0;
            }
        }
        let mut matched_terms = vec![];
        for t in &query.terms {
            let s = term_score(&n, &p, t);
            if s > 0.0 {
                matched_terms.push(t.clone());
                score += s;
            }
        }
        let matched = matched_terms.len();
        if matched > 0 || query.terms.is_empty() {
            let exact = query.words.iter().filter(|w| holds(&p.text, w)).count();
            let exact_whole = query.whole.as_ref().is_some_and(|w| holds(&p.text, w));
            let snip = snippet(&n, &query);
            let exact_identifier = query.words.iter().any(|w| tokenize(w).len() > 1 && holds(&p.text, w));
            let slug = n.slug();
            let own = query.terms.iter().any(|t| t.len() >= crate::id::MIN_PREFIX && n.id.starts_with(t.as_str())) || query.words.iter().any(|w| slug.as_deref() == Some(w.as_str()));
            hits.push(Hit { note: n, score, snippet: snip, matched, matched_terms, own, exact, exact_whole, exact_identifier, lapsed: None });
        }
    }
    let terms = query.terms.len();
    let best = hits.iter().map(|h| h.matched).max().unwrap_or(0);
    let floor = best.saturating_sub(terms / 4).max(1.min(terms));
    hits.retain(|h| h.matched >= floor || h.exact_identifier);
    let rank = |h: &Hit| h.score * kind_weight(&h.note.kind) * recency(&h.note);
    hits.sort_by(|a, b| {
        (b.own, b.exact_whole, b.exact, b.matched)
            .cmp(&(a.own, a.exact_whole, a.exact, a.matched))
            .then(rank(b).partial_cmp(&rank(a)).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.note.updated.cmp(&a.note.updated))
    });
    SearchResult { total: hits.len(), fell_back_to_or: !hits.is_empty() && best < terms, hits, terms }
}
