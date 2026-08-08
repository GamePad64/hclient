import re, sys, pathlib

ROOT = pathlib.Path("crates/http-ng-wasi/src")

# Terminal forms that lose Err — regardless of whether they sit
# RIGHT AFTER `.set_x(..)` or after an arbitrarily long chain of
# transparent combinators (`.map(..)`, `.map_err(..)`,
# `.and_then(..)`, ...) — finding 2 round 3, see `walk_chain`.
TERMINAL_DISCARD_NO_ARGS = {"ok", "is_ok", "is_err", "unwrap_or_default"}
TERMINAL_DISCARD_WITH_ARGS = {"unwrap_or", "unwrap_or_else", "map_or"}

# `(:[^=;{}]*)?` — an optional type annotation between `_<ident>`
# and `=` (`let _: Result<(), E> = ..`), otherwise it wouldn't match.
LET_UNDERSCORE = re.compile(r'\blet\s+_\w*\s*(:[^=;{}]*)?=\s*[^;{}]*$')
DROP_CALL = re.compile(r'\bdrop\s*\([^;{}]*$')
# A bare `_ = expr;` (destructuring assignment, not `let`). The `\b`
# before `_` avoids catching the end of an ordinary identifier like
# `foo_ = ..` (there's no word boundary between `o` and `_` — both
# are word characters); `(?!>)` avoids catching `_ => ..` (a match
# arm).
BARE_UNDERSCORE_ASSIGN = re.compile(r'\b_\s*=(?!>)\s*[^;{}]*$')
# `if let Err(_e) = ..` — including inside a let-chain (`if let
# Some(x) = y && let Err(_e) = ..`), where `if` doesn't sit
# immediately before `let Err` — hence no `\bif\s+` at the start,
# we look for `let Err(_ident) =` itself anywhere in the prefix.
# By itself this does not mean a violation — see the empty-block
# check below (finding 1 round 3).
IF_LET_ERR_UNDERSCORE = re.compile(r'\blet\s+Err\s*\(\s*_\w*\s*\)\s*=\s*[^;{}]*$')
BLOCK_COMMENT = re.compile(r'/\*.*?\*/', re.DOTALL)

def strip_block_comments(text):
    # Replaces the contents of `/* .. */` with spaces, PRESERVING
    # newlines inside — line numbers for everything after don't
    # shift. Doesn't handle nested `/* /* */ */` (Rust allows this)
    # — a deliberately unclosed gap, a rare case for this file.
    def repl(m):
        return ''.join(c if c == '\n' else ' ' for c in m.group(0))
    return BLOCK_COMMENT.sub(repl, text)

CHAR_LITERAL = re.compile(r"'(\\u\{[0-9a-fA-F]+\}|\\.|[^'\\])'")

def strip_line_comment(line):
    # A naive `line.split("//", 1)[0]` would cut the line at `//`
    # INSIDE a string literal too (`"http://x"`), hiding everything
    # further along the same line, including a real discard.
    # `set_path_with_query`/`set_authority` in this crate genuinely
    # accept strings of this shape — not a hypothetical risk.
    in_string = False
    i, n = 0, len(line)
    while i < n:
        c = line[i]
        if in_string:
            if c == '\\' and i + 1 < n:
                i += 2
                continue
            if c == '"':
                in_string = False
            i += 1
            continue
        if c == '"':
            in_string = True
            i += 1
            continue
        if c == "'":
            # Could be a character literal ('x', '\n', '"',
            # '\u{1F600}') or a lifetime ('a, 'static) — we skip a
            # character literal whole, so that a `"` inside it
            # ('"') doesn't desync the string tracker above
            # (a round 3 finding, found by our own testing, not by
            # the review — a false positive: without this,
            # commented-out code after '"' on the same line would
            # read as real). A lifetime isn't closed by `'`, the
            # regex won't match, a lone `'` passes through as an
            # ordinary character.
            m = CHAR_LITERAL.match(line, i)
            if m:
                i = m.end()
                continue
            i += 1
            continue
        if c == '/' and i + 1 < n and line[i + 1] == '/':
            return line[:i]
        i += 1
    return line

def match_parens(stream, open_paren_pos):
    depth = 0
    i = open_paren_pos
    n = len(stream)
    while i < n:
        if stream[i] == '(':
            depth += 1
        elif stream[i] == ')':
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None

def walk_chain(stream, pos):
    # Starting at `pos` (right after the closing paren of
    # `.set_x(..)`), walks the `.method(args)...` chain until it
    # hits `?` (propagation — nothing further counts) or a
    # non-chain character. Returns (method_names,
    # chain_end_position, propagated).
    methods = []
    i, n = pos, len(stream)
    while i < n:
        while i < n and stream[i].isspace():
            i += 1
        if i < n and stream[i] == '?':
            return methods, i + 1, True
        if i < n and stream[i] == '.':
            j = i + 1
            name_start = j
            while j < n and (stream[j].isalnum() or stream[j] == '_'):
                j += 1
            name = stream[name_start:j]
            while j < n and stream[j].isspace():
                j += 1
            if j < n and stream[j] == '(':
                close = match_parens(stream, j)
                if close is None:
                    return methods, i, False
                methods.append(name)
                i = close + 1
                continue
            return methods, i, False
        break
    return methods, i, False

violations = []

for path in sorted(ROOT.rglob("*.rs")):
    text = strip_block_comments(path.read_text())
    lines = text.splitlines()
    stream_chars = []
    char_line = []
    for lineno, line in enumerate(lines, start=1):
        code = strip_line_comment(line)
        for ch in code:
            stream_chars.append(ch)
            char_line.append(lineno)
        stream_chars.append(' ')
        char_line.append(lineno)
    stream = ''.join(stream_chars)

    for m in re.finditer(r'\.set_[a-z_]+\(', stream):
        start = m.start()
        end = match_parens(stream, m.end() - 1)
        if end is None:
            continue
        line_no = char_line[start]
        snippet = re.sub(r'\s+', ' ', stream[start:end + 1]).strip()

        methods, chain_end, propagated = walk_chain(stream, end + 1)

        discard_method = next(
            (
                name
                for name in methods
                if name in TERMINAL_DISCARD_NO_ARGS or name in TERMINAL_DISCARD_WITH_ARGS
            ),
            None,
        )
        if discard_method is not None:
            chain_text = re.sub(r'\s+', ' ', stream[end + 1:chain_end]).strip()
            violations.append(
                (path, line_no, f"{snippet}{chain_text} [terminal: .{discard_method}(..)]")
            )
            continue

        if propagated:
            continue

        stmt_start = max(
            stream.rfind(';', 0, start),
            stream.rfind('{', 0, start),
            stream.rfind('}', 0, start),
        ) + 1
        prefix = stream[stmt_start:start]

        matched_prefix = None
        for pattern in (LET_UNDERSCORE, DROP_CALL, BARE_UNDERSCORE_ASSIGN):
            if pattern.search(prefix):
                matched_prefix = pattern
                break
        if matched_prefix is not None:
            p = re.sub(r'\s+', ' ', prefix).strip()
            violations.append((path, line_no, f"{p[-40:]} {snippet}"))
            continue

        # Finding 1 round 3: we search for `{` starting at
        # `chain_end`, not `end` — so that
        # `if let Err(_e) = opts.set_x(y).map_err(..) { }` also
        # finds the CORRECT brace, not one of the braces inside
        # `map_err`. Flag only if the content between `{` and its
        # matching `}` is whitespace only: a non-empty block
        # (`{ return Err(_e.into()); }`) is honest propagation,
        # `_e` is used, not discarded.
        if IF_LET_ERR_UNDERSCORE.search(prefix):
            brace_pos = stream.find('{', chain_end)
            if brace_pos != -1:
                depth = 0
                j = brace_pos
                close_pos = None
                while j < len(stream):
                    if stream[j] == '{':
                        depth += 1
                    elif stream[j] == '}':
                        depth -= 1
                        if depth == 0:
                            close_pos = j
                            break
                    j += 1
                if close_pos is not None and stream[brace_pos + 1:close_pos].strip() == '':
                    p = re.sub(r'\s+', ' ', prefix).strip()
                    violations.append((path, line_no, f"{p[-40:]} {snippet} {{ }}"))

for path, line_no, snippet in violations:
    print(f"{path}:{line_no}: {snippet}")

if violations:
    print(
        "::error::found a discarded Result from a wasi:http setter "
        "(let _ / let _ident / let _: Type / .ok() / .unwrap_or_default() / "
        ".unwrap_or(..) / .unwrap_or_else(..) / .map_or(..) / .is_ok() / .is_err() "
        "— at any position in a combinator chain, not only right after the call — "
        "/ drop(..) / a bare `_ = ..` / `if let Err(_e) = ..` with an empty block — "
        "on one line or via a rustfmt wrap) — every host refusal must "
        "become a typed Error, see crates/http-ng-wasi/src/convert.rs"
    )
    sys.exit(1)

print(f"no discarded wasi:http setter results ({len(list(ROOT.rglob('*.rs')))} files scanned)")
