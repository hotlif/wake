use wake_common::Span;
use wake_css_in_js::{CssTemplate, CssTemplateKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSegment {
    pub host: Span,
    pub virtual_css: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualCssDocument {
    pub kind: CssTemplateKind,
    pub text: String,
    pub template_span: Span,
    pub body_span: Span,
    pub segments: Vec<SourceSegment>,
    pub interpolations: Vec<Span>,
}

impl VirtualCssDocument {
    pub fn from_template(source: &str, template: &CssTemplate) -> Self {
        let (prefix, suffix) = match template.kind {
            CssTemplateKind::Css => (".__crab__ {\n", "\n}"),
            CssTemplateKind::Keyframes => ("@keyframes __crab__ {\n", "\n}"),
            CssTemplateKind::GlobalStyle => ("", ""),
        };
        let body_span = template
            .literal_spans
            .first()
            .zip(template.literal_spans.last())
            .map_or(
                Span::at(template.template_span.lo.saturating_add(1)),
                |(first, last)| Span::new(first.lo, last.hi),
            );
        let mut body = source
            .as_bytes()
            .get(body_span.lo as usize..body_span.hi as usize)
            .map_or_else(Vec::new, |slice| {
                slice
                    .iter()
                    .map(|byte| {
                        if matches!(byte, b'\r' | b'\n') {
                            *byte
                        } else {
                            b' '
                        }
                    })
                    .collect()
            });
        let prefix_len = prefix.len() as u32;
        let mut segments = Vec::with_capacity(template.literal_spans.len());
        for literal in &template.literal_spans {
            if literal.lo < body_span.lo || literal.hi > body_span.hi {
                continue;
            }
            let source_slice = &source.as_bytes()[literal.lo as usize..literal.hi as usize];
            let body_lo = (literal.lo - body_span.lo) as usize;
            let body_hi = body_lo + source_slice.len();
            body[body_lo..body_hi].copy_from_slice(source_slice);
            segments.push(SourceSegment {
                host: *literal,
                virtual_css: Span::new(
                    prefix_len + literal.lo - body_span.lo,
                    prefix_len + literal.hi - body_span.lo,
                ),
            });
        }
        let mut text = String::with_capacity(prefix.len() + body.len() + suffix.len());
        text.push_str(prefix);
        // Literal bytes are copied from valid UTF-8 and interpolation bytes become ASCII spaces.
        text.push_str(std::str::from_utf8(&body).expect("virtual CSS remains valid UTF-8"));
        text.push_str(suffix);
        Self {
            kind: template.kind,
            text,
            template_span: template.template_span,
            body_span,
            segments,
            interpolations: template.interpolations.clone(),
        }
    }

    pub fn host_to_virtual_offset(&self, offset: u32) -> Option<u32> {
        self.segments.iter().find_map(|segment| {
            (segment.host.lo <= offset && offset <= segment.host.hi)
                .then_some(segment.virtual_css.lo + offset - segment.host.lo)
        })
    }

    pub fn virtual_to_host_offset(&self, offset: u32) -> Option<u32> {
        self.segments.iter().find_map(|segment| {
            (segment.virtual_css.lo <= offset && offset <= segment.virtual_css.hi)
                .then_some(segment.host.lo + offset - segment.virtual_css.lo)
        })
    }

    pub fn host_to_virtual_span(&self, span: Span) -> Option<Span> {
        self.segments.iter().find_map(|segment| {
            segment.host.contains(span).then_some(Span::new(
                segment.virtual_css.lo + span.lo - segment.host.lo,
                segment.virtual_css.lo + span.hi - segment.host.lo,
            ))
        })
    }

    pub fn virtual_to_host_span(&self, span: Span) -> Option<Span> {
        self.segments.iter().find_map(|segment| {
            segment.virtual_css.contains(span).then_some(Span::new(
                segment.host.lo + span.lo - segment.virtual_css.lo,
                segment.host.lo + span.hi - segment.virtual_css.lo,
            ))
        })
    }

    pub fn contains_host_offset(&self, offset: u32) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.host.lo <= offset && offset <= segment.host.hi)
    }

    pub fn edit_is_safe(&self, span: Span) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.host.contains(span))
            && self
                .interpolations
                .iter()
                .all(|interpolation| interpolation.hi <= span.lo || span.hi <= interpolation.lo)
    }
}
