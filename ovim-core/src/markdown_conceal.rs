//! Lightweight markdown conceal helpers used by the renderer.

/// A span of source bytes to replace with a string in display space.
#[derive(Debug, Clone)]
pub struct ConcealSpan {
    pub src_start: usize,
    pub src_end: usize,
    pub replacement: String,
    /// The URL target for link conceal spans (None for images and non-link spans).
    pub url: Option<String>,
}

/// Result of conceal application.
#[derive(Debug, Clone)]
pub struct LineTransform {
    pub text: String,
    /// src byte offset -> view char index (len = src.len()+1)
    pub src_to_view: Vec<usize>,
}

fn map_source_char(src_to_view: &mut [usize], byte_idx: usize, ch: char, view_idx: usize) {
    for mapped in src_to_view.iter_mut().skip(byte_idx).take(ch.len_utf8()) {
        *mapped = view_idx;
    }
}

impl LineTransform {
    pub fn identity(src: &str) -> Self {
        let mut src_to_view = vec![0; src.len() + 1];
        let mut view_idx = 0;
        for (byte_idx, ch) in src.char_indices() {
            map_source_char(&mut src_to_view, byte_idx, ch, view_idx);
            view_idx += 1;
        }
        src_to_view[src.len()] = view_idx;
        Self {
            text: src.to_string(),
            src_to_view,
        }
    }
}

/// Apply non-overlapping conceal spans to `src`.
pub fn apply_conceal(src: &str, spans: &[ConcealSpan]) -> LineTransform {
    if spans.is_empty() {
        return LineTransform::identity(src);
    }

    let mut out = String::new();
    let mut src_to_view = vec![0usize; src.len() + 1];
    let mut view_idx = 0usize;
    let mut cursor = 0usize;

    for span in spans {
        let start = span.src_start.min(src.len());
        let end = span.src_end.min(src.len());

        // copy text before the span
        if cursor < start {
            for (b, ch) in src[cursor..start].char_indices() {
                let byte_idx = cursor + b;
                map_source_char(&mut src_to_view, byte_idx, ch, view_idx);
                out.push(ch);
                view_idx += 1;
            }
            cursor = start;
        }

        // map concealed bytes to current view index (start of replacement)
        for item in src_to_view.iter_mut().take(end).skip(cursor) {
            *item = view_idx;
        }

        for ch in span.replacement.chars() {
            out.push(ch);
            view_idx += 1;
        }

        cursor = end;
    }

    // tail
    if cursor < src.len() {
        for (b, ch) in src[cursor..].char_indices() {
            let byte_idx = cursor + b;
            map_source_char(&mut src_to_view, byte_idx, ch, view_idx);
            out.push(ch);
            view_idx += 1;
        }
        cursor = src.len();
    }

    src_to_view[cursor] = view_idx;

    LineTransform {
        text: out,
        src_to_view,
    }
}

/// Simple link/image conceal scanner.
pub fn scan_markdown_conceal(line: &str) -> Vec<ConcealSpan> {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'[' {
            if let Some(close) = line[i + 1..].find(']') {
                let label_end = i + 1 + close;
                if label_end + 1 < len && bytes[label_end + 1] == b'(' {
                    if let Some(paren) = line[label_end + 2..].find(')') {
                        let url_end = label_end + 2 + paren;
                        let label = &line[i + 1..label_end];
                        let url = line[label_end + 2..url_end].to_string();
                        spans.push(ConcealSpan {
                            src_start: i,
                            src_end: url_end + 1,
                            replacement: label.to_string(),
                            url: Some(url),
                        });
                        i = url_end + 1;
                        continue;
                    }
                }
            }
        } else if bytes[i] == b'!' && i + 1 < len && bytes[i + 1] == b'[' {
            if let Some(close) = line[i + 2..].find(']') {
                let label_end = i + 2 + close;
                if label_end + 1 < len && bytes[label_end + 1] == b'(' {
                    if let Some(paren) = line[label_end + 2..].find(')') {
                        let url_end = label_end + 2 + paren;
                        let alt = line[i + 2..label_end].trim();
                        let replacement = if alt.is_empty() {
                            "⧉".to_string()
                        } else {
                            format!("⧉ {}", alt)
                        };
                        spans.push(ConcealSpan {
                            src_start: i,
                            src_end: url_end + 1,
                            replacement,
                            url: None,
                        });
                        i = url_end + 1;
                        continue;
                    }
                }
            }
        } else if bytes[i] == b'<' {
            if let Some(close) = line[i + 1..].find('>') {
                let end = i + 1 + close;
                let inner = &line[i + 1..end];
                if inner.starts_with("http://") || inner.starts_with("https://") {
                    let display = inner
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/');
                    spans.push(ConcealSpan {
                        src_start: i,
                        src_end: end + 1,
                        replacement: display.to_string(),
                        url: Some(inner.to_string()),
                    });
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    spans
}

/// Inverse of a [`LineTransform`]: map a character index in the *view*
/// (concealed) text back to a character column in the *source* text.
///
/// Used by mouse hit-testing: the user clicks at a view column, but the cursor
/// must be placed in source space. A view position that falls inside a concealed
/// span's replacement maps to the **start** of that span (where the line reveals
/// itself for editing); positions in un-concealed runs map 1:1.
pub fn view_char_to_src_char(src: &str, transform: &LineTransform, view_char: usize) -> usize {
    // `src_to_view` is non-decreasing in source byte offset. Walk source chars
    // and find the last one whose view index is `<= view_char`, then back up to
    // the first char sharing that same view index — that first char is the
    // start of the concealed span (or the char itself, in a 1:1 run).
    let chars: Vec<usize> = src.char_indices().map(|(b, _)| b).collect();
    let view_at = |char_idx: usize| -> usize {
        let byte = chars.get(char_idx).copied().unwrap_or(src.len());
        transform
            .src_to_view
            .get(byte)
            .copied()
            .unwrap_or_else(|| transform.src_to_view.last().copied().unwrap_or(0))
    };

    let mut last: Option<usize> = None;
    for ci in 0..chars.len() {
        if view_at(ci) <= view_char {
            last = Some(ci);
        } else {
            break;
        }
    }

    match last {
        None => 0,
        Some(ci) => {
            let v = view_at(ci);
            let mut first = ci;
            while first > 0 && view_at(first - 1) == v {
                first -= 1;
            }
            first
        }
    }
}

/// A concealed link's position in view space and its target URL.
#[derive(Debug, Clone)]
pub struct ConcealedLink {
    /// Start character index in the view (concealed) text.
    pub view_start: usize,
    /// End character index (exclusive) in the view text.
    pub view_end: usize,
    /// The URL this link points to.
    pub url: String,
}

/// Extract link ranges in view-space from conceal spans and their transform.
pub fn extract_concealed_links(
    spans: &[ConcealSpan],
    transform: &LineTransform,
) -> Vec<ConcealedLink> {
    let mut links = Vec::new();
    for span in spans {
        if let Some(ref url) = span.url {
            let view_start = transform
                .src_to_view
                .get(span.src_start)
                .or_else(|| transform.src_to_view.last())
                .copied()
                .unwrap_or(0);
            let replacement_len = span.replacement.chars().count();
            let view_end = view_start + replacement_len;
            links.push(ConcealedLink {
                view_start,
                view_end,
                url: url.clone(),
            });
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conceal_link() {
        let spans = scan_markdown_conceal("[label](http://example.com)");
        let t = apply_conceal("[label](http://example.com)", &spans);
        assert_eq!(t.text, "label");
        // first byte maps to view 0
        assert_eq!(t.src_to_view[0], 0);
    }

    #[test]
    fn identity_maps_every_utf8_byte_to_its_view_character() {
        let transform = LineTransform::identity("éx");

        assert_eq!(transform.src_to_view, vec![0, 0, 1, 2]);
    }

    #[test]
    fn conceal_maps_multibyte_prefix_bytes_consistently() {
        let src = "é [x](url)";
        let spans = scan_markdown_conceal(src);
        let transform = apply_conceal(src, &spans);

        assert_eq!(transform.text, "é x");
        assert_eq!(transform.src_to_view[0], 0);
        assert_eq!(transform.src_to_view[1], 0);
        assert_eq!(transform.src_to_view[2], 1);
    }

    #[test]
    fn concealed_link_extraction_tolerates_an_empty_transform_map() {
        let spans = vec![ConcealSpan {
            src_start: 4,
            src_end: 8,
            replacement: "link".to_string(),
            url: Some("https://example.com".to_string()),
        }];
        let transform = LineTransform {
            text: String::new(),
            src_to_view: Vec::new(),
        };

        let links = extract_concealed_links(&spans, &transform);
        assert_eq!(links[0].view_start, 0);
        assert_eq!(links[0].view_end, 4);
    }

    #[test]
    fn view_char_to_src_char_maps_label_to_span_start() {
        let src = "pre [clickme](http://example.com) post";
        let spans = scan_markdown_conceal(src);
        let t = apply_conceal(src, &spans);
        // View text is "pre clickme post".
        assert_eq!(t.text, "pre clickme post");
        // "pre " is a 1:1 run → maps straight through.
        assert_eq!(view_char_to_src_char(src, &t, 0), 0); // 'p'
        assert_eq!(view_char_to_src_char(src, &t, 2), 2); // 'e'
                                                          // Anywhere inside the "clickme" label (view 4..11) → start of the span,
                                                          // i.e. the '[' at source char 4.
        assert_eq!(view_char_to_src_char(src, &t, 4), 4);
        assert_eq!(view_char_to_src_char(src, &t, 7), 4);
        assert_eq!(view_char_to_src_char(src, &t, 10), 4);
        // The space before "post" in view space → its source char (after the
        // whole concealed span).
        let space_view = "pre clickme".chars().count(); // 11
        let src_space = src.find(") post").unwrap() + 1; // byte of the space
        let src_space_char = src[..src_space].chars().count();
        assert_eq!(view_char_to_src_char(src, &t, space_view), src_space_char);
    }

    #[test]
    fn conceal_image_alt() {
        let spans = scan_markdown_conceal("![alt text](img.png)");
        let t = apply_conceal("![alt text](img.png)", &spans);
        assert!(t.text.starts_with("⧉ alt"));
    }
}
