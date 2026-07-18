use dash_mpd::{AdaptationSet, Representation};

/// Substitution values for DASH `SegmentTemplate` URL identifiers (ISO 23009-1 §5.3.9.4.4).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TemplateVars<'a> {
    pub representation_id: &'a str,
    pub bandwidth: Option<u64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub frame_rate: Option<&'a str>,
    pub ext: Option<&'a str>,
    pub initialization: Option<&'a str>,
    pub number: Option<u64>,
    pub time: Option<u64>,
    pub sub_number: Option<u64>,
}

#[allow(clippy::derivable_impls)]
impl<'a> Default for TemplateVars<'a> {
    fn default() -> Self {
        Self {
            representation_id: "",
            bandwidth: None,
            width: None,
            height: None,
            frame_rate: None,
            ext: None,
            initialization: None,
            number: None,
            time: None,
            sub_number: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateIdent {
    RepId,
    Number,
    Bandwidth,
    Time,
    SubNumber,
    Width,
    Height,
    FrameRate,
    Ext,
    Initialization,
}

pub(super) fn infer_template_ext(
    rep: &Representation,
    adaptation_set: &AdaptationSet,
) -> &'static str {
    let mime = rep
        .mimeType
        .as_deref()
        .or(adaptation_set.mimeType.as_deref())
        .unwrap_or("");
    let mime_lower = mime.to_ascii_lowercase();
    if mime_lower.contains("/webm") || mime_lower.contains("matroska") {
        "webm"
    } else if mime_lower.contains("mp2t") {
        "ts"
    } else if mime_lower == "image/jpeg" || mime_lower == "image/jpg" {
        "jpg"
    } else if mime_lower == "image/png" {
        "png"
    } else if mime_lower == "image/bmp" || mime_lower == "image/x-ms-bmp" {
        "bmp"
    } else {
        "m4s"
    }
}

/// Parse `%0[width]d` width from a DASH format tag; defaults to 1 per spec when absent or invalid.
fn dash_format_width(format: Option<&str>) -> usize {
    let Some(fmt) = format else {
        return 1;
    };
    if !fmt.starts_with("%0") || !fmt.ends_with('d') || fmt.len() < 4 {
        return 1;
    }
    let width_str = &fmt[2..fmt.len() - 1];
    if width_str.is_empty() {
        return 1;
    }
    width_str.parse::<usize>().unwrap_or(1).max(1)
}

fn format_dash_integer(value: u64, width: usize) -> String {
    format!("{:0width$}", value, width = width.max(1))
}

pub(super) fn template_contains_number_or_time_ident(template: &str) -> bool {
    let mut rest = template;
    while !rest.is_empty() {
        let Some(dollar_pos) = rest.find('$') else {
            break;
        };
        rest = &rest[dollar_pos..];

        if rest.starts_with("$$") {
            rest = &rest[2..];
            continue;
        }

        let Some(close) = rest[1..].find('$') else {
            break;
        };

        let token = &rest[1..=close];
        if let Some((ident, _)) = parse_template_ident(token) {
            if matches!(ident, TemplateIdent::Number | TemplateIdent::Time) {
                return true;
            }
        }
        rest = &rest[close + 2..];
    }
    false
}

fn parse_template_ident(token: &str) -> Option<(TemplateIdent, Option<&str>)> {
    let (name, format) = match token.find('%') {
        Some(pos) => (&token[..pos], Some(&token[pos..])),
        None => (token, None),
    };
    let ident = match name {
        "RepresentationID" => TemplateIdent::RepId,
        "Number" => TemplateIdent::Number,
        "Bandwidth" => TemplateIdent::Bandwidth,
        "Time" => TemplateIdent::Time,
        "SubNumber" => TemplateIdent::SubNumber,
        "Width" => TemplateIdent::Width,
        "Height" => TemplateIdent::Height,
        "FrameRate" => TemplateIdent::FrameRate,
        "Ext" => TemplateIdent::Ext,
        "Initialization" => TemplateIdent::Initialization,
        _ => return None,
    };
    if matches!(
        ident,
        TemplateIdent::RepId
            | TemplateIdent::FrameRate
            | TemplateIdent::Ext
            | TemplateIdent::Initialization
    ) && format.is_some()
    {
        return None;
    }
    Some((ident, format))
}

fn resolve_template_ident(
    ident: TemplateIdent,
    format: Option<&str>,
    vars: &TemplateVars<'_>,
) -> Option<String> {
    match ident {
        TemplateIdent::RepId => Some(vars.representation_id.to_string()),
        TemplateIdent::Number => vars
            .number
            .map(|n| format_dash_integer(n, dash_format_width(format))),
        TemplateIdent::Bandwidth => vars
            .bandwidth
            .map(|bw| format_dash_integer(bw, dash_format_width(format))),
        TemplateIdent::Time => vars
            .time
            .map(|t| format_dash_integer(t, dash_format_width(format))),
        TemplateIdent::SubNumber => Some(format_dash_integer(
            vars.sub_number.unwrap_or(1),
            dash_format_width(format),
        )),
        TemplateIdent::Width => vars
            .width
            .map(|w| format_dash_integer(w, dash_format_width(format))),
        TemplateIdent::Height => vars
            .height
            .map(|h| format_dash_integer(h, dash_format_width(format))),
        TemplateIdent::FrameRate => vars.frame_rate.map(str::to_string),
        TemplateIdent::Ext => vars.ext.map(str::to_string),
        TemplateIdent::Initialization => vars.initialization.map(str::to_string),
    }
}

/// DASH `$...$` template interpolation (§5.3.9.4.4), including `$SubNumber$` (§5.3.9.6.5).
pub(crate) fn interpolate_template(template: &str, vars: &TemplateVars<'_>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        let Some(dollar_pos) = rest.find('$') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..dollar_pos]);
        rest = &rest[dollar_pos..];

        if rest.starts_with("$$") {
            out.push('$');
            rest = &rest[2..];
            continue;
        }

        let Some(close) = rest[1..].find('$') else {
            out.push('$');
            rest = &rest[1..];
            continue;
        };

        let token = &rest[1..=close];
        let consumed = close + 2;
        if let Some((ident, format)) = parse_template_ident(token) {
            if let Some(value) = resolve_template_ident(ident, format, vars) {
                out.push_str(&value);
                rest = &rest[consumed..];
                continue;
            }
        }

        out.push('$');
        rest = &rest[1..];
    }
    out
}

/// Build template substitution values for one representation (init/media URL construction).
pub(crate) fn template_vars_for_representation<'a>(
    rep: &'a Representation,
    adaptation_set: &'a AdaptationSet,
) -> TemplateVars<'a> {
    TemplateVars {
        representation_id: rep.id.as_deref().unwrap_or_default(),
        bandwidth: rep.bandwidth,
        width: rep.width.or(adaptation_set.width),
        height: rep.height.or(adaptation_set.height),
        frame_rate: rep
            .frameRate
            .as_deref()
            .or(adaptation_set.frameRate.as_deref()),
        ext: Some(infer_template_ext(rep, adaptation_set)),
        initialization: None,
        number: None,
        time: None,
        sub_number: None,
    }
}
