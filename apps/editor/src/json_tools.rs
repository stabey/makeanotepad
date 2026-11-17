use std::fmt;
use std::ops::Range;

use editor_services::LineEnding;
use ropey::Rope;

/// Maximum input byte size accepted by JSON/JSONC validation and transform commands (`4 MiB`).
pub const MAX_JSON_TOOL_BYTES: usize = 4 * 1024 * 1024;

/// Maximum container nesting depth permitted by the heap-stack parser before returning a syntax error.
pub const MAX_JSON_DEPTH: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonDialect {
    Json,
    Jsonc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonIndent {
    Spaces(u8),
    Tabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonTransformMode {
    Format,
    Minify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonValidationSummary {
    pub token_count: usize,
    pub max_depth: usize,
    pub has_comments: bool,
    pub has_trailing_commas: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonError {
    ByteBudgetExceeded {
        bytes: usize,
        max_bytes: usize,
    },
    Syntax {
        message: String,
        byte_offset: usize,
        line_1based: usize,
        col_1based: usize,
    },
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ByteBudgetExceeded { bytes, max_bytes } => write!(
                f,
                "JSON document size ({bytes} B) exceeds the {max_bytes} B tool budget."
            ),
            Self::Syntax {
                message,
                byte_offset,
                line_1based,
                col_1based,
            } => write!(
                f,
                "JSON syntax error at line {line_1based}, column {col_1based} (byte {byte_offset}): {message}"
            ),
        }
    }
}

impl std::error::Error for JsonError {}

/// Validates `src` against strict RFC 8259 (`JsonDialect::Json`) or JSON with comments and
/// trailing commas (`JsonDialect::Jsonc`) without materializing parsed number or string values.
pub fn validate_json_str(
    src: &str,
    dialect: JsonDialect,
) -> Result<JsonValidationSummary, JsonError> {
    check_byte_budget(src.len())?;
    let mut sink = NoopSink;
    parse_tokens(src, dialect, &mut sink)
}

/// Validates and losslessly pretty-prints `src`, preserving all raw string escapes, exact number
/// text, duplicate object keys, trailing newline presence, and (in JSONC) comments and trailing commas.
pub fn format_json_str(
    src: &str,
    dialect: JsonDialect,
    indent: JsonIndent,
    line_ending: LineEnding,
) -> Result<String, JsonError> {
    check_byte_budget(src.len())?;
    let mut tokens = Vec::new();
    let _summary = parse_tokens(src, dialect, &mut tokens)?;
    let eol = line_ending_str(line_ending);
    let unit = indent_unit(indent);
    let had_trailing_newline = src.ends_with('\n') || src.ends_with('\r');
    Ok(emit_formatted(
        src,
        &tokens,
        unit,
        eol,
        had_trailing_newline,
    ))
}

/// Validates and losslessly minifies `src`, preserving exact raw scalar tokens and (in JSONC)
/// comments and trailing commas while inserting mandatory line breaks after `//` line comments.
pub fn minify_json_str(
    src: &str,
    dialect: JsonDialect,
    line_ending: LineEnding,
) -> Result<String, JsonError> {
    check_byte_budget(src.len())?;
    let eol = line_ending_str(line_ending);
    let mut sink = MinifySink {
        src,
        eol,
        out: String::with_capacity(src.len()),
        prev_was_line_comment: false,
    };
    let _summary = parse_tokens(src, dialect, &mut sink)?;
    Ok(sink.out)
}

/// Plans a minimal UTF-8- and CRLF-safe replacement edit on `rope` for formatting or minification.
///
/// Checks `rope.len() > MAX_JSON_TOOL_BYTES` before converting the rope to a contiguous `String`,
/// returns `Ok(None)` if the transformed output is byte-identical to `rope`, and adjusts common
/// prefix/suffix bounds so neither `start` nor `end` splits a multi-byte UTF-8 sequence or `\r\n`.
pub fn plan_rope_transform(
    rope: &Rope,
    dialect: JsonDialect,
    mode: JsonTransformMode,
    indent: JsonIndent,
    line_ending: LineEnding,
) -> Result<Option<(Range<usize>, String)>, JsonError> {
    check_byte_budget(rope.len())?;

    let mut original = String::with_capacity(rope.len());
    for chunk in rope.chunks() {
        original.push_str(chunk);
    }

    let transformed = match mode {
        JsonTransformMode::Format => format_json_str(&original, dialect, indent, line_ending)?,
        JsonTransformMode::Minify => minify_json_str(&original, dialect, line_ending)?,
    };

    if original == transformed {
        return Ok(None);
    }

    let orig = original.as_bytes();
    let next = transformed.as_bytes();
    let min_len = orig.len().min(next.len());

    let mut prefix = 0usize;
    while prefix < min_len && orig[prefix] == next[prefix] {
        prefix += 1;
    }
    while prefix > 0
        && (!original.is_char_boundary(prefix)
            || !transformed.is_char_boundary(prefix)
            || splits_crlf_bytes(orig, prefix)
            || splits_crlf_bytes(next, prefix))
    {
        prefix -= 1;
    }

    let remaining = min_len - prefix;
    let mut suffix = 0usize;
    while suffix < remaining && orig[orig.len() - 1 - suffix] == next[next.len() - 1 - suffix] {
        suffix += 1;
    }

    let mut orig_end = orig.len() - suffix;
    let mut next_end = next.len() - suffix;
    while orig_end < orig.len()
        && (!original.is_char_boundary(orig_end)
            || !transformed.is_char_boundary(next_end)
            || splits_crlf_bytes(orig, orig_end)
            || splits_crlf_bytes(next, next_end))
    {
        orig_end += 1;
        next_end += 1;
    }

    Ok(Some((
        prefix..orig_end,
        transformed[prefix..next_end].to_string(),
    )))
}

fn check_byte_budget(bytes: usize) -> Result<(), JsonError> {
    if bytes > MAX_JSON_TOOL_BYTES {
        return Err(JsonError::ByteBudgetExceeded {
            bytes,
            max_bytes: MAX_JSON_TOOL_BYTES,
        });
    }
    Ok(())
}

fn line_ending_str(line_ending: LineEnding) -> &'static str {
    match line_ending {
        LineEnding::Crlf => "\r\n",
        LineEnding::Lf | LineEnding::MixedReadOnly => "\n",
    }
}

fn indent_unit(indent: JsonIndent) -> &'static str {
    match indent {
        JsonIndent::Tabs => "\t",
        JsonIndent::Spaces(n) => {
            let spaces = if n == 0 { 2 } else { n.clamp(1, 16) } as usize;
            &"                "[..spaces]
        }
    }
}

fn splits_crlf_bytes(bytes: &[u8], offset: usize) -> bool {
    offset > 0 && offset < bytes.len() && bytes[offset - 1] == b'\r' && bytes[offset] == b'\n'
}

fn line_col_at(src: &str, byte_offset: usize) -> (usize, usize) {
    let mut target = byte_offset.min(src.len());
    while target > 0 && !src.is_char_boundary(target) {
        target -= 1;
    }

    let bytes = src.as_bytes();
    let mut line = 1usize;
    let mut col = 1usize;
    let mut i = 0usize;

    while i < target {
        let b = bytes[i];
        if b == b'\r' {
            if i + 1 < target && bytes[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            line += 1;
            col = 1;
        } else if b == b'\n' {
            i += 1;
            line += 1;
            col = 1;
        } else if b < 0x80 {
            i += 1;
            col += 1;
        } else {
            let ch = src[i..target].chars().next().expect("valid UTF-8 char");
            i += ch.len_utf8();
            col += 1;
        }
    }

    (line, col)
}

fn syntax_error(src: &str, byte_offset: usize, message: impl Into<String>) -> JsonError {
    let (line_1based, col_1based) = line_col_at(src, byte_offset);
    JsonError::Syntax {
        message: message.into(),
        byte_offset,
        line_1based,
        col_1based,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Key,
    String,
    Number,
    True,
    False,
    Null,
    LineComment,
    BlockComment,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: TokenKind,
    start: u32,
    end: u32,
    had_newline_before: bool,
}

trait TokenSink {
    fn push(&mut self, token: Token);
}

struct NoopSink;

impl TokenSink for NoopSink {
    fn push(&mut self, _token: Token) {}
}

impl TokenSink for Vec<Token> {
    fn push(&mut self, token: Token) {
        Vec::push(self, token);
    }
}

struct MinifySink<'a> {
    src: &'a str,
    eol: &'static str,
    out: String,
    prev_was_line_comment: bool,
}

impl TokenSink for MinifySink<'_> {
    fn push(&mut self, token: Token) {
        if self.prev_was_line_comment {
            self.out.push_str(self.eol);
            self.prev_was_line_comment = false;
        }
        let slice = &self.src[token.start as usize..token.end as usize];
        match token.kind {
            TokenKind::LineComment => {
                self.out.push_str(slice);
                self.prev_was_line_comment = true;
            }
            TokenKind::BlockComment => {
                push_normalized_block_comment(&mut self.out, slice, self.eol);
            }
            _ => {
                self.out.push_str(slice);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerState {
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expecting {
    RootValue,
    ArrayValue { after_comma: bool },
    ArrayCommaOrClose,
    ObjectKey { after_comma: bool },
    ObjectColon,
    ObjectValue,
    ObjectCommaOrClose,
    AfterRoot,
}

fn parse_tokens<S: TokenSink>(
    src: &str,
    dialect: JsonDialect,
    sink: &mut S,
) -> Result<JsonValidationSummary, JsonError> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut pos = 0usize;
    let mut stack: Vec<ContainerState> = Vec::new();
    let mut expecting = Expecting::RootValue;
    let mut last_comma_offset = 0usize;

    let mut token_count = 0usize;
    let mut max_depth = 0usize;
    let mut has_comments = false;
    let mut has_trailing_commas = false;

    loop {
        let mut had_newline_before = false;
        loop {
            while pos < len {
                match bytes[pos] {
                    b' ' | b'\t' => pos += 1,
                    b'\n' | b'\r' => {
                        had_newline_before = true;
                        pos += 1;
                    }
                    _ => break,
                }
            }

            if pos < len && bytes[pos] == b'/' {
                let comment_start = pos;
                match bytes.get(pos + 1).copied() {
                    Some(b'/') => {
                        if dialect == JsonDialect::Json {
                            return Err(syntax_error(
                                src,
                                comment_start,
                                "line comments ('//') are not allowed in strict JSON",
                            ));
                        }
                        has_comments = true;
                        pos += 2;
                        while pos < len && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
                            pos += 1;
                        }
                        sink.push(Token {
                            kind: TokenKind::LineComment,
                            start: comment_start as u32,
                            end: pos as u32,
                            had_newline_before,
                        });
                        // Reset newline tracking after emitting a comment so the next token
                        // records whether a newline separates it from this comment.
                        had_newline_before = false;
                        continue;
                    }
                    Some(b'*') => {
                        if dialect == JsonDialect::Json {
                            return Err(syntax_error(
                                src,
                                comment_start,
                                "block comments ('/* ... */') are not allowed in strict JSON",
                            ));
                        }
                        has_comments = true;
                        pos += 2;
                        let mut closed = false;
                        while pos + 1 < len {
                            if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                                pos += 2;
                                closed = true;
                                break;
                            }
                            pos += 1;
                        }
                        if !closed {
                            return Err(syntax_error(
                                src,
                                comment_start,
                                "unterminated block comment",
                            ));
                        }
                        sink.push(Token {
                            kind: TokenKind::BlockComment,
                            start: comment_start as u32,
                            end: pos as u32,
                            had_newline_before,
                        });
                        had_newline_before = false;
                        continue;
                    }
                    _ => {
                        return Err(syntax_error(src, comment_start, "unexpected character '/'"));
                    }
                }
            }

            break;
        }

        if pos >= len {
            return match expecting {
                Expecting::AfterRoot => Ok(JsonValidationSummary {
                    token_count,
                    max_depth,
                    has_comments,
                    has_trailing_commas,
                }),
                Expecting::RootValue => {
                    Err(syntax_error(src, pos, "expected a JSON value, found EOF"))
                }
                _ => Err(syntax_error(
                    src,
                    pos,
                    "unexpected end of JSON input inside open container",
                )),
            };
        }

        if expecting == Expecting::AfterRoot {
            return Err(syntax_error(
                src,
                pos,
                "unexpected trailing token after JSON root value",
            ));
        }

        let tok_start = pos;
        let b = bytes[pos];

        match b {
            b'{' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected '{'"));
                }
                if stack.len() >= MAX_JSON_DEPTH {
                    return Err(syntax_error(
                        src,
                        tok_start,
                        format!("maximum JSON nesting depth ({MAX_JSON_DEPTH}) exceeded"),
                    ));
                }
                pos += 1;
                stack.push(ContainerState::Object);
                max_depth = max_depth.max(stack.len());
                expecting = Expecting::ObjectKey { after_comma: false };
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::LBrace,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'[' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected '['"));
                }
                if stack.len() >= MAX_JSON_DEPTH {
                    return Err(syntax_error(
                        src,
                        tok_start,
                        format!("maximum JSON nesting depth ({MAX_JSON_DEPTH}) exceeded"),
                    ));
                }
                pos += 1;
                stack.push(ContainerState::Array);
                max_depth = max_depth.max(stack.len());
                expecting = Expecting::ArrayValue { after_comma: false };
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::LBracket,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'}' => {
                match expecting {
                    Expecting::ObjectKey { after_comma: false } | Expecting::ObjectCommaOrClose => {
                    }
                    Expecting::ObjectKey { after_comma: true } => {
                        if dialect == JsonDialect::Json {
                            return Err(syntax_error(
                                src,
                                last_comma_offset,
                                "trailing comma in object is not allowed in strict JSON",
                            ));
                        }
                        has_trailing_commas = true;
                    }
                    _ => return Err(syntax_error(src, tok_start, "unexpected '}'")),
                }
                pos += 1;
                stack.pop();
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::RBrace,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b']' => {
                match expecting {
                    Expecting::ArrayValue { after_comma: false } | Expecting::ArrayCommaOrClose => {
                    }
                    Expecting::ArrayValue { after_comma: true } => {
                        if dialect == JsonDialect::Json {
                            return Err(syntax_error(
                                src,
                                last_comma_offset,
                                "trailing comma in array is not allowed in strict JSON",
                            ));
                        }
                        has_trailing_commas = true;
                    }
                    _ => return Err(syntax_error(src, tok_start, "unexpected ']'")),
                }
                pos += 1;
                stack.pop();
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::RBracket,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b':' => {
                if expecting != Expecting::ObjectColon {
                    return Err(syntax_error(src, tok_start, "unexpected ':'"));
                }
                pos += 1;
                expecting = Expecting::ObjectValue;
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::Colon,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b',' => {
                match expecting {
                    Expecting::ArrayCommaOrClose => {
                        last_comma_offset = tok_start;
                        expecting = Expecting::ArrayValue { after_comma: true };
                    }
                    Expecting::ObjectCommaOrClose => {
                        last_comma_offset = tok_start;
                        expecting = Expecting::ObjectKey { after_comma: true };
                    }
                    _ => return Err(syntax_error(src, tok_start, "unexpected ','")),
                }
                pos += 1;
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::Comma,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'"' => {
                let is_key = matches!(expecting, Expecting::ObjectKey { .. });
                if !is_key
                    && !matches!(
                        expecting,
                        Expecting::RootValue
                            | Expecting::ArrayValue { .. }
                            | Expecting::ObjectValue
                    )
                {
                    return Err(syntax_error(src, tok_start, "unexpected string literal"));
                }
                pos = scan_string(src, tok_start)?;
                expecting = if is_key {
                    Expecting::ObjectColon
                } else {
                    after_value_expectation(&stack)
                };
                token_count += 1;
                sink.push(Token {
                    kind: if is_key {
                        TokenKind::Key
                    } else {
                        TokenKind::String
                    },
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'-' | b'0'..=b'9' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected number literal"));
                }
                pos = scan_number(src, tok_start)?;
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::Number,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b't' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected literal"));
                }
                if !bytes[pos..].starts_with(b"true") {
                    return Err(syntax_error(
                        src,
                        tok_start,
                        "invalid literal (expected 'true')",
                    ));
                }
                pos += 4;
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::True,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'f' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected literal"));
                }
                if !bytes[pos..].starts_with(b"false") {
                    return Err(syntax_error(
                        src,
                        tok_start,
                        "invalid literal (expected 'false')",
                    ));
                }
                pos += 5;
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::False,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            b'n' => {
                if !matches!(
                    expecting,
                    Expecting::RootValue | Expecting::ArrayValue { .. } | Expecting::ObjectValue
                ) {
                    return Err(syntax_error(src, tok_start, "unexpected literal"));
                }
                if !bytes[pos..].starts_with(b"null") {
                    return Err(syntax_error(
                        src,
                        tok_start,
                        "invalid literal (expected 'null')",
                    ));
                }
                pos += 4;
                expecting = after_value_expectation(&stack);
                token_count += 1;
                sink.push(Token {
                    kind: TokenKind::Null,
                    start: tok_start as u32,
                    end: pos as u32,
                    had_newline_before,
                });
            }
            _ => {
                let ch = src[tok_start..].chars().next().expect("valid UTF-8 char");
                return Err(syntax_error(
                    src,
                    tok_start,
                    format!("unexpected character '{}'", ch.escape_default()),
                ));
            }
        }
    }
}

fn after_value_expectation(stack: &[ContainerState]) -> Expecting {
    match stack.last().copied() {
        None => Expecting::AfterRoot,
        Some(ContainerState::Array) => Expecting::ArrayCommaOrClose,
        Some(ContainerState::Object) => Expecting::ObjectCommaOrClose,
    }
}

fn scan_string(src: &str, quote_start: usize) -> Result<usize, JsonError> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut pos = quote_start + 1;

    while pos < len {
        let b = bytes[pos];
        if b == b'"' {
            return Ok(pos + 1);
        }
        if b < 0x20 {
            return Err(syntax_error(
                src,
                pos,
                format!("unescaped control character 0x{b:02x} inside JSON string"),
            ));
        }
        if b == b'\\' {
            let esc_pos = pos;
            pos += 1;
            if pos >= len {
                return Err(syntax_error(
                    src,
                    quote_start,
                    "unterminated escape sequence in JSON string",
                ));
            }
            match bytes[pos] {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                    pos += 1;
                }
                b'u' => {
                    pos += 1;
                    if pos + 4 > len {
                        return Err(syntax_error(
                            src,
                            esc_pos,
                            "incomplete unicode escape sequence in JSON string",
                        ));
                    }
                    for hex_i in 0..4 {
                        if !bytes[pos + hex_i].is_ascii_hexdigit() {
                            return Err(syntax_error(
                                src,
                                pos + hex_i,
                                "invalid hex digit in '\\uXXXX' escape sequence",
                            ));
                        }
                    }
                    pos += 4;
                }
                other => {
                    return Err(syntax_error(
                        src,
                        esc_pos,
                        format!("invalid escape '\\{}' in JSON string", other as char),
                    ));
                }
            }
            continue;
        }
        pos += 1;
    }

    Err(syntax_error(
        src,
        quote_start,
        "unterminated JSON string literal",
    ))
}

fn scan_number(src: &str, num_start: usize) -> Result<usize, JsonError> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut pos = num_start;

    if bytes[pos] == b'-' {
        pos += 1;
        if pos >= len || !bytes[pos].is_ascii_digit() {
            return Err(syntax_error(
                src,
                num_start,
                "expected digit after '-' in JSON number",
            ));
        }
    }

    if bytes[pos] == b'0' {
        pos += 1;
        if pos < len && bytes[pos].is_ascii_digit() {
            return Err(syntax_error(
                src,
                pos,
                "leading zeros are not allowed in JSON numbers",
            ));
        }
    } else if bytes[pos].is_ascii_digit() {
        pos += 1;
        while pos < len && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
    } else {
        return Err(syntax_error(src, pos, "invalid JSON number"));
    }

    if pos < len && bytes[pos] == b'.' {
        pos += 1;
        if pos >= len || !bytes[pos].is_ascii_digit() {
            return Err(syntax_error(
                src,
                pos,
                "expected digit after decimal point in JSON number",
            ));
        }
        while pos < len && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
    }

    if pos < len && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        pos += 1;
        if pos < len && (bytes[pos] == b'+' || bytes[pos] == b'-') {
            pos += 1;
        }
        if pos >= len || !bytes[pos].is_ascii_digit() {
            return Err(syntax_error(
                src,
                pos,
                "expected digit in JSON number exponent",
            ));
        }
        while pos < len && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
    }

    Ok(pos)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineAnchor {
    StartOfLine,
    OpenContainer,
    Key,
    Colon,
    Value,
    Comma,
    Standalone,
}

struct FormatContainer {
    has_values: bool,
}

fn emit_formatted(
    src: &str,
    tokens: &[Token],
    unit: &str,
    eol: &str,
    had_trailing_newline: bool,
) -> String {
    let mut out = String::with_capacity(src.len() + src.len() / 4);
    let mut stack: Vec<FormatContainer> = Vec::new();
    let mut anchor = LineAnchor::StartOfLine;
    let mut must_break = false;
    let mut idx = 0usize;

    while idx < tokens.len() {
        let tok = tokens[idx];
        let raw = &src[tok.start as usize..tok.end as usize];

        match tok.kind {
            TokenKind::LineComment | TokenKind::BlockComment => {
                let is_line_comment = tok.kind == TokenKind::LineComment;
                let is_multiline_block =
                    !is_line_comment && (raw.contains('\n') || raw.contains('\r'));

                let mut inline =
                    !must_break && !tok.had_newline_before && anchor != LineAnchor::StartOfLine;
                if inline && anchor == LineAnchor::OpenContainer {
                    let container_has_values = stack.last().is_some_and(|c| c.has_values);
                    if !container_has_values || is_multiline_block {
                        inline = false;
                    }
                }

                if inline {
                    out.push(' ');
                    if is_line_comment {
                        out.push_str(raw);
                    } else {
                        push_normalized_block_comment(&mut out, raw, eol);
                    }
                    must_break = is_line_comment || is_multiline_block;
                } else {
                    if anchor != LineAnchor::StartOfLine {
                        out.push_str(eol);
                    }
                    push_indent(&mut out, unit, stack.len());
                    if is_line_comment {
                        out.push_str(raw);
                    } else {
                        push_normalized_block_comment(&mut out, raw, eol);
                    }
                    anchor = LineAnchor::Standalone;
                    must_break = is_line_comment || is_multiline_block;
                }
                idx += 1;
            }
            TokenKind::LBrace | TokenKind::LBracket => {
                position_for_value_or_key(&mut out, unit, eol, stack.len(), anchor, must_break);
                let close_kind = if tok.kind == TokenKind::LBrace {
                    TokenKind::RBrace
                } else {
                    TokenKind::RBracket
                };
                if tokens
                    .get(idx + 1)
                    .is_some_and(|next| next.kind == close_kind)
                {
                    out.push_str(if tok.kind == TokenKind::LBrace {
                        "{}"
                    } else {
                        "[]"
                    });
                    anchor = LineAnchor::Value;
                    must_break = false;
                    idx += 2;
                } else {
                    let has_values = container_has_value_tokens(&tokens[idx + 1..], close_kind);
                    out.push_str(raw);
                    stack.push(FormatContainer { has_values });
                    anchor = LineAnchor::OpenContainer;
                    must_break = false;
                    idx += 1;
                }
            }
            TokenKind::RBrace | TokenKind::RBracket => {
                stack.pop();
                if anchor != LineAnchor::StartOfLine {
                    out.push_str(eol);
                }
                push_indent(&mut out, unit, stack.len());
                out.push_str(raw);
                anchor = LineAnchor::Value;
                must_break = false;
                idx += 1;
            }
            TokenKind::Colon => {
                if anchor == LineAnchor::Key && !must_break {
                    out.push(':');
                } else {
                    if anchor != LineAnchor::StartOfLine {
                        out.push_str(eol);
                    }
                    push_indent(&mut out, unit, stack.len());
                    out.push(':');
                }
                anchor = LineAnchor::Colon;
                must_break = false;
                idx += 1;
            }
            TokenKind::Comma => {
                if anchor == LineAnchor::Value && !must_break {
                    out.push(',');
                } else {
                    if anchor != LineAnchor::StartOfLine {
                        out.push_str(eol);
                    }
                    push_indent(&mut out, unit, stack.len());
                    out.push(',');
                }
                anchor = LineAnchor::Comma;
                must_break = false;
                idx += 1;
            }
            TokenKind::Key => {
                position_for_value_or_key(&mut out, unit, eol, stack.len(), anchor, must_break);
                out.push_str(raw);
                anchor = LineAnchor::Key;
                must_break = false;
                idx += 1;
            }
            TokenKind::String
            | TokenKind::Number
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null => {
                position_for_value_or_key(&mut out, unit, eol, stack.len(), anchor, must_break);
                out.push_str(raw);
                anchor = LineAnchor::Value;
                must_break = false;
                idx += 1;
            }
        }
    }

    if had_trailing_newline {
        out.push_str(eol);
    }

    out
}

fn container_has_value_tokens(after_open: &[Token], close_kind: TokenKind) -> bool {
    for tok in after_open {
        match tok.kind {
            TokenKind::LineComment | TokenKind::BlockComment => continue,
            kind => return kind != close_kind,
        }
    }
    false
}

fn position_for_value_or_key(
    out: &mut String,
    unit: &str,
    eol: &str,
    depth: usize,
    anchor: LineAnchor,
    must_break: bool,
) {
    if anchor == LineAnchor::StartOfLine {
        push_indent(out, unit, depth);
    } else if anchor == LineAnchor::Colon && !must_break {
        out.push(' ');
    } else {
        out.push_str(eol);
        push_indent(out, unit, depth);
    }
}

fn push_indent(out: &mut String, unit: &str, depth: usize) {
    for _ in 0..depth {
        out.push_str(unit);
    }
}

fn push_normalized_block_comment(out: &mut String, raw: &str, eol: &str) {
    let bytes = raw.as_bytes();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            out.push_str(&raw[seg_start..i]);
            out.push_str(eol);
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            seg_start = i;
        } else if bytes[i] == b'\n' {
            out.push_str(&raw[seg_start..i]);
            out.push_str(eol);
            i += 1;
            seg_start = i;
        } else {
            i += 1;
        }
    }
    if seg_start < raw.len() {
        out.push_str(&raw[seg_start..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_planned_edit(rope: &Rope, range: Range<usize>, replacement: &str) -> Rope {
        assert!(rope.is_char_boundary(range.start));
        assert!(rope.is_char_boundary(range.end));
        assert!(!splits_crlf_at(rope, range.start));
        assert!(!splits_crlf_at(rope, range.end));
        let mut next = rope.clone();
        next.try_remove(range.clone()).expect("valid remove range");
        next.try_insert(range.start, replacement)
            .expect("valid insert range");
        next
    }

    fn splits_crlf_at(rope: &Rope, offset: usize) -> bool {
        offset > 0
            && offset < rope.len()
            && rope.byte(offset - 1) == b'\r'
            && rope.byte(offset) == b'\n'
    }

    #[test]
    fn lossless_numbers_escapes_duplicate_keys_and_unicode() {
        let src = concat!(
            "{\n",
            "  \"big\": 900719925474099312345678901234567890,\n",
            "  \"neg_zero\": -0,\n",
            "  \"exp\": 1e+05,\n",
            "  \"frac\": 0.00100,\n",
            "  \"esc\": \"\\u0041\\uD83D\\uDE00\\\"\\\\\\/\\b\\f\\n\\r\\t\",\n",
            "  \"dup\": 1,\n",
            "  \"dup\": \"second\",\n",
            "  \"键🔑\": \"你好😀世界\"\n",
            "}\n",
        );

        let summary = validate_json_str(src, JsonDialect::Json).expect("valid strict JSON");
        assert!(!summary.has_comments);
        assert!(!summary.has_trailing_commas);
        assert_eq!(summary.max_depth, 1);

        let formatted = format_json_str(
            src,
            JsonDialect::Json,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("format succeeds");
        assert_eq!(formatted, src);

        let minified =
            minify_json_str(src, JsonDialect::Json, LineEnding::Lf).expect("minify succeeds");
        assert_eq!(
            minified,
            "{\"big\":900719925474099312345678901234567890,\"neg_zero\":-0,\"exp\":1e+05,\"frac\":0.00100,\"esc\":\"\\u0041\\uD83D\\uDE00\\\"\\\\\\/\\b\\f\\n\\r\\t\",\"dup\":1,\"dup\":\"second\",\"键🔑\":\"你好😀世界\"}"
        );
    }

    #[test]
    fn empty_containers_and_indent_variants() {
        let src = "{ \"a\": {   }, \"b\": [\n\n], \"c\": [1, 2] }";
        let formatted_2 = format_json_str(
            src,
            JsonDialect::Json,
            JsonIndent::Spaces(0),
            LineEnding::Lf,
        )
        .expect("format Spaces(0)");
        assert_eq!(
            formatted_2,
            "{\n  \"a\": {},\n  \"b\": [],\n  \"c\": [\n    1,\n    2\n  ]\n}"
        );

        let formatted_tabs =
            format_json_str(src, JsonDialect::Json, JsonIndent::Tabs, LineEnding::Lf)
                .expect("format Tabs");
        assert_eq!(
            formatted_tabs,
            "{\n\t\"a\": {},\n\t\"b\": [],\n\t\"c\": [\n\t\t1,\n\t\t2\n\t]\n}"
        );

        let formatted_clamped = format_json_str(
            src,
            JsonDialect::Json,
            JsonIndent::Spaces(40),
            LineEnding::Lf,
        )
        .expect("format clamped Spaces");
        let indent_16 = "                ";
        assert!(formatted_clamped.contains(&format!("\n{indent_16}\"a\": {{}}")));
    }

    #[test]
    fn strict_json_rejects_comments_trailing_commas_and_invalid_tokens() {
        let line_comment = "{\n  // not allowed\n  \"a\": 1\n}";
        let err = validate_json_str(line_comment, JsonDialect::Json).unwrap_err();
        assert!(err.to_string().contains("line 2, column 3"));
        match err {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(byte_offset, 4);
                assert_eq!(line_1based, 2);
                assert_eq!(col_1based, 3);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let block_comment = "{\"a\": /* c */ 1}";
        match validate_json_str(block_comment, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(byte_offset, 6);
                assert_eq!(line_1based, 1);
                assert_eq!(col_1based, 7);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let trailing_arr = "[1,]";
        match validate_json_str(trailing_arr, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(byte_offset, 2);
                assert_eq!(line_1based, 1);
                assert_eq!(col_1based, 3);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // `{` (1) + `\r\n` (2) + `  ` (2) + `"` (1) + `😀` (4) + `"` (1) + `:` (1) + ` ` (1) + `1` (1) = 14
        let trailing_obj = "{\r\n  \"😀\": 1,\r\n}";
        match validate_json_str(trailing_obj, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(byte_offset, 14);
                assert_eq!(line_1based, 2);
                assert_eq!(col_1based, 9);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // Reject invalid numbers, escapes, BOM at 0, control chars, trailing root tokens.
        for bad in [
            "",
            "   \n",
            "\u{FEFF}{}",
            "01",
            "-01",
            "+1",
            "1.",
            "1e",
            "1e+",
            "NaN",
            "Infinity",
            "\"bad\nline\"",
            "\"bad\\x41\"",
            "\"bad\\u123\"",
            "\"bad\\u123g\"",
            "\"unclosed",
            "{} {}",
            "{\"a\": 1, \"b\": 2,,}",
            "[1, , 2]",
            "/* unterminated */",
        ] {
            assert!(
                validate_json_str(bad, JsonDialect::Json).is_err(),
                "expected strict JSON rejection for {bad:?}"
            );
        }

        // Scalar column accuracy with CR-only line endings.
        let cr_err = "1\r{\"😀\": 01}";
        match validate_json_str(cr_err, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(line_1based, 2);
                assert_eq!(col_1based, 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let cr_err_2 = "{\r\"😀\": 01}";
        match validate_json_str(cr_err_2, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                // `{` (1) + `\r` (1) + `"` (1) + `😀` (4) + `"` (1) + `:` (1) + ` ` (1) + `0` (1) = 11
                assert_eq!(byte_offset, 11);
                assert_eq!(line_1based, 2);
                assert_eq!(col_1based, 7);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn jsonc_preserves_comments_and_trailing_commas_in_format_and_minify() {
        let src = concat!(
            "// header comment\n",
            "{ // open inline\n",
            "  // standalone before a\n",
            "  \"a\": 1, // inline after comma\n",
            "  \"b\": /* inline val */ 2 // before close\n",
            "  ,\n",
            "  \"c\": [\n",
            "    /* line1\r\n",
            "     * line2 */\n",
            "    10,\n",
            "  ],\n",
            "  \"empty_with_comment\": { /* keep me */ }\n",
            "} // trailing root\n",
        );

        let summary = validate_json_str(src, JsonDialect::Jsonc).expect("valid JSONC");
        assert!(summary.has_comments);
        assert!(summary.has_trailing_commas);

        let formatted = format_json_str(
            src,
            JsonDialect::Jsonc,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("JSONC format succeeds");
        assert!(formatted.contains("{\n    /* keep me */\n  }"));
        assert!(formatted.contains("\"b\": /* inline val */ 2 // before close\n  ,"));
        assert!(formatted.contains("/* line1\n     * line2 */"));
        assert!(formatted.ends_with("} // trailing root\n"));

        // Formatting must remain idempotent and keep validating as JSONC.
        let formatted_twice = format_json_str(
            &formatted,
            JsonDialect::Jsonc,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("re-format succeeds");
        assert_eq!(formatted_twice, formatted);
        validate_json_str(&formatted, JsonDialect::Jsonc).expect("formatted JSONC stays valid");

        // Line comment right before closing brace always gets a line break before `}`.
        let inline_before_brace = "{\"a\": 1 // end\n}";
        let fmt_brace = format_json_str(
            inline_before_brace,
            JsonDialect::Jsonc,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("format line comment before close");
        assert_eq!(fmt_brace, "{\n  \"a\": 1 // end\n}");
        validate_json_str(&fmt_brace, JsonDialect::Jsonc).expect("valid JSONC");

        // Minify preserves comments and inserts mandatory EOL after `//` comments.
        let minified =
            minify_json_str(src, JsonDialect::Jsonc, LineEnding::Lf).expect("minify JSONC");
        assert!(minified.contains("// header comment\n{"));
        assert!(
            minified.contains(
                "\"a\":1,// inline after comma\n\"b\":/* inline val */2// before close\n,"
            )
        );
        assert!(minified.contains("/* line1\n     * line2 */"));
        assert!(minified.ends_with("}// trailing root"));
        validate_json_str(&minified, JsonDialect::Jsonc).expect("minified JSONC stays valid");

        // JSONC still rejects double commas and unterminated block comments.
        assert!(validate_json_str("[1, , 2]", JsonDialect::Jsonc).is_err());
        assert!(validate_json_str("{\"a\": 1,,}", JsonDialect::Jsonc).is_err());
        assert!(validate_json_str("{\"a\": /* unclosed }", JsonDialect::Jsonc).is_err());
        assert!(validate_json_str("// comment only\n", JsonDialect::Jsonc).is_err());
    }

    #[test]
    fn crlf_preservation_and_crlf_safe_rope_planning() {
        let crlf_formatted = "{\r\n  \"a\": 1,\r\n  \"b\": [\r\n    2,\r\n    3\r\n  ]\r\n}\r\n";
        let rope_ok = Rope::from_str(crlf_formatted);
        let idempotent = plan_rope_transform(
            &rope_ok,
            JsonDialect::Json,
            JsonTransformMode::Format,
            JsonIndent::Spaces(2),
            LineEnding::Crlf,
        )
        .expect("idempotent plan succeeds");
        assert_eq!(idempotent, None);

        // Mixed LF/CRLF ending right before `}`: common suffix matches `\n}`, which would split
        // `\r\n` unless adjusted out of the `\r\n` pair!
        let mixed_to_lf = Rope::from_str("{\n  \"a\": 1\r\n}");
        let (range_lf, repl_lf) = plan_rope_transform(
            &mixed_to_lf,
            JsonDialect::Json,
            JsonTransformMode::Format,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("plan succeeds")
        .expect("edit needed");
        let after_lf = apply_planned_edit(&mixed_to_lf, range_lf, &repl_lf);
        assert_eq!(after_lf.to_string(), "{\n  \"a\": 1\n}");

        // Reverse direction: LF -> CRLF where `\n}` is common suffix in target `\r\n}`.
        let lf_to_crlf = Rope::from_str("{\n  \"a\": 1\n}");
        let (range_crlf, repl_crlf) = plan_rope_transform(
            &lf_to_crlf,
            JsonDialect::Json,
            JsonTransformMode::Format,
            JsonIndent::Spaces(2),
            LineEnding::Crlf,
        )
        .expect("plan succeeds")
        .expect("edit needed");
        let after_crlf = apply_planned_edit(&lf_to_crlf, range_crlf, &repl_crlf);
        assert_eq!(after_crlf.to_string(), "{\r\n  \"a\": 1\r\n}");

        // Minify with CRLF after line comment in JSONC.
        let jsonc_rope = Rope::from_str("{\r\n  \"😀\": 1 // keep\r\n}\r\n");
        let (min_range, min_repl) = plan_rope_transform(
            &jsonc_rope,
            JsonDialect::Jsonc,
            JsonTransformMode::Minify,
            JsonIndent::Spaces(2),
            LineEnding::Crlf,
        )
        .expect("minify plan succeeds")
        .expect("edit needed");
        let after_min = apply_planned_edit(&jsonc_rope, min_range, &min_repl);
        assert_eq!(after_min.to_string(), "{\"😀\":1// keep\r\n}");
    }

    #[test]
    fn byte_budget_and_deep_nesting_protection() {
        let over_str = format!("\"{}\"", "a".repeat(MAX_JSON_TOOL_BYTES));
        assert!(over_str.len() > MAX_JSON_TOOL_BYTES);
        let budget_err = validate_json_str(&over_str, JsonDialect::Json).unwrap_err();
        assert_eq!(
            budget_err,
            JsonError::ByteBudgetExceeded {
                bytes: over_str.len(),
                max_bytes: MAX_JSON_TOOL_BYTES,
            }
        );
        assert!(budget_err.to_string().contains("exceeds"));
        let _: &dyn std::error::Error = &budget_err;

        let over_rope = Rope::from_str(&over_str);
        assert_eq!(
            plan_rope_transform(
                &over_rope,
                JsonDialect::Json,
                JsonTransformMode::Format,
                JsonIndent::Spaces(2),
                LineEnding::Lf,
            ),
            Err(JsonError::ByteBudgetExceeded {
                bytes: over_str.len(),
                max_bytes: MAX_JSON_TOOL_BYTES,
            })
        );

        // Exactly MAX_JSON_DEPTH (2048) levels must succeed without stack overflow.
        let exact_depth = format!(
            "{}1{}",
            "[".repeat(MAX_JSON_DEPTH),
            "]".repeat(MAX_JSON_DEPTH)
        );
        let summary = validate_json_str(&exact_depth, JsonDialect::Json)
            .expect("2048 nesting levels should succeed");
        assert_eq!(summary.max_depth, MAX_JSON_DEPTH);

        let formatted_deep = format_json_str(
            &exact_depth,
            JsonDialect::Json,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("formatting 2048 levels should succeed without stack overflow");
        assert!(formatted_deep.len() > MAX_JSON_TOOL_BYTES);

        // Smaller deep nesting (1400 levels) stays under 4 MiB even when formatted and round-trips.
        let depth_1400 = format!("{}1{}", "[".repeat(1400), "]".repeat(1400));
        let fmt_1400 = format_json_str(
            &depth_1400,
            JsonDialect::Json,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("formatting 1400 levels succeeds");
        let summary_1400 = validate_json_str(&fmt_1400, JsonDialect::Json)
            .expect("formatted 1400-level JSON validates");
        assert_eq!(summary_1400.max_depth, 1400);
        assert_eq!(
            minify_json_str(&fmt_1400, JsonDialect::Json, LineEnding::Lf).unwrap(),
            depth_1400
        );

        // Exceeding MAX_JSON_DEPTH (> 2048) returns clean Syntax error.
        let too_deep = format!(
            "{}1{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        match validate_json_str(&too_deep, JsonDialect::Json).unwrap_err() {
            JsonError::Syntax {
                byte_offset,
                line_1based,
                col_1based,
                ..
            } => {
                assert_eq!(byte_offset, MAX_JSON_DEPTH);
                assert_eq!(line_1based, 1);
                assert_eq!(col_1based, MAX_JSON_DEPTH + 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // Object nesting also obeys MAX_JSON_DEPTH without recursion.
        let obj_2048 = format!(
            "{}1{}",
            "{\"a\":".repeat(MAX_JSON_DEPTH),
            "}".repeat(MAX_JSON_DEPTH)
        );
        assert_eq!(
            validate_json_str(&obj_2048, JsonDialect::Json)
                .expect("2048 object depth succeeds")
                .max_depth,
            MAX_JSON_DEPTH
        );
        let obj_2049 = format!(
            "{}1{}",
            "{\"a\":".repeat(MAX_JSON_DEPTH + 1),
            "}".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(validate_json_str(&obj_2049, JsonDialect::Json).is_err());
    }

    #[test]
    fn scalar_roots_strings_with_comment_like_text_and_jsonc_edge_cases() {
        for (input, expected_tokens) in [
            ("true", 1),
            ("false", 1),
            ("null", 1),
            ("-0", 1),
            ("\"https://example.com/*not-a-comment*/\"", 1),
        ] {
            let summary = validate_json_str(input, JsonDialect::Json).expect("valid scalar root");
            assert_eq!(summary.token_count, expected_tokens);
            assert_eq!(summary.max_depth, 0);
            assert!(!summary.has_comments);
            assert_eq!(
                format_json_str(
                    input,
                    JsonDialect::Json,
                    JsonIndent::Spaces(2),
                    LineEnding::MixedReadOnly
                )
                .unwrap(),
                input
            );
            assert_eq!(
                format_json_str(
                    &format!("{input}\n"),
                    JsonDialect::Json,
                    JsonIndent::Spaces(2),
                    LineEnding::MixedReadOnly
                )
                .unwrap(),
                format!("{input}\n")
            );
        }

        let tricky_jsonc = [
            "/* lead */ /* lead2 */ {}",
            "{\"a\" /* k */ : /* v */ 1 /* c */,}",
            "[1 // c1\n, 2]",
            "{\"a\": // colon c\n 1}",
            "{ /* a\nb */ \"x\": 1 }",
            "{ /* a */ /* b */ }",
            "// top\n42 // tail",
        ];
        for sample in tricky_jsonc {
            let formatted = format_json_str(
                sample,
                JsonDialect::Jsonc,
                JsonIndent::Spaces(2),
                LineEnding::Lf,
            )
            .expect("format tricky JSONC");
            let formatted_again = format_json_str(
                &formatted,
                JsonDialect::Jsonc,
                JsonIndent::Spaces(2),
                LineEnding::Lf,
            )
            .expect("reformat tricky JSONC");
            assert_eq!(formatted_again, formatted, "not idempotent for: {sample}");
            validate_json_str(&formatted, JsonDialect::Jsonc).expect("formatted stays valid");

            let minified =
                minify_json_str(sample, JsonDialect::Jsonc, LineEnding::Lf).expect("minify JSONC");
            let minified_again = minify_json_str(&minified, JsonDialect::Jsonc, LineEnding::Lf)
                .expect("re-minify JSONC");
            assert_eq!(minified_again, minified);
            validate_json_str(&minified, JsonDialect::Jsonc).expect("minified stays valid");
        }

        // Large multi-chunk rope partial edit plans only the changed slice.
        let mut big = String::from("{\n");
        for i in 0..500 {
            big.push_str(&format!("  \"k{i}\": {i},\n"));
        }
        big.push_str("  \"target\":    123\n}\n");
        let big_rope = Rope::from_str(&big);
        let (range, repl) = plan_rope_transform(
            &big_rope,
            JsonDialect::Json,
            JsonTransformMode::Format,
            JsonIndent::Spaces(2),
            LineEnding::Lf,
        )
        .expect("plan succeeds")
        .expect("edit needed");
        assert!(range.len() < 16);
        let applied = apply_planned_edit(&big_rope, range, &repl);
        assert_eq!(
            applied.to_string(),
            format_json_str(
                &big,
                JsonDialect::Json,
                JsonIndent::Spaces(2),
                LineEnding::Lf
            )
            .unwrap()
        );

        // Invalid rope returns syntax error cleanly.
        let invalid_rope = Rope::from_str("{\"a\": [1,]}");
        assert!(
            plan_rope_transform(
                &invalid_rope,
                JsonDialect::Json,
                JsonTransformMode::Format,
                JsonIndent::Spaces(2),
                LineEnding::Lf,
            )
            .is_err()
        );
    }
}
