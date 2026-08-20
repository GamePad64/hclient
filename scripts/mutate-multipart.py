#!/usr/bin/env python3
"""Mutation harness for the multipart work. Temporary; not committed."""
import subprocess, sys, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
M = "crates/hclient/src/multipart.rs"
R = "crates/hclient/src/request.rs"

# (id, file, find, replace, description)
MUTATIONS = [
    ("m1", M, 'let v = if token {', 'let v = if false {',
     "content_type: always quote the boundary"),
    ("m2", M, 'format!("multipart/form-data; boundary=\\"{}\\"", self.0)',
     'format!("multipart/form-data; boundary={}", self.0)',
     "content_type: never quote, even where a token character is missing"),
    ("m3", M, '        getrandom::fill(&mut raw)?;', '        if false { getrandom::fill(&mut raw)?; }',
     "random: a fixed boundary (the all-zero draw sse.rs's jitter falls back to)"),
    ("m4", M, '        let mut raw = [0u8; 16];', '        let mut raw = [0u8; 8];',
     "random: 64 bits instead of 128"),
    ("m5", M, "            && !value.ends_with(' ')", "            && true",
     "Boundary::new: drop RFC 2046's 'not a trailing space'"),
    ("m6", M, '        let ok = (1..=70).contains(&value.len())',
     '        let ok = (1..=71).contains(&value.len())',
     "Boundary::new: allow 71 characters"),
    ("m7", M, "            '\"' => out.push_str(\"%22\"),", "            '\"' => out.push('\"'),",
     "escape: leave a quote as itself"),
    ("m8", M, "            '\\n' => out.push_str(\"%0A\"),\n            '\\r' => out.push_str(\"%0D\"),",
     "            '\\n' => out.push('\\n'),\n            '\\r' => out.push('\\r'),",
     "escape: leave CR and LF as themselves (the injection)"),
    ("m9", M, "            '\\t' => out.push('\\t'),",
     "            '%' => out.push_str(\"%25\"),\n            '\\t' => out.push('\\t'),",
     "escape: also escape '%', which no browser does"),
    ("m10", M, "                return Err(MultipartError::ControlByte {\n                    field,\n                    byte: c as u8,\n                });",
     "                out.push(c);",
     "escape: pass other C0 controls through instead of refusing"),
    ("m11", M, '        out.push_str("\\r\\n");\n        Ok(Bytes::from(out.into_bytes()))',
     '        Ok(Bytes::from(out.into_bytes()))',
     "head: drop the blank line that ends a part's headers"),
    ("m12", M, '            out.push_str("; filename=\\"");',
     '            out.push_str("; filename*=UTF-8\'\'x; filename=\\"");',
     "head: add the filename* RFC 7578 4.2 forbids"),
    ("m13", M, '            segments.push(Segment::Bytes(Bytes::from_static(b"\\r\\n")));',
     '            segments.push(Segment::Bytes(Bytes::new()));',
     "encode: drop the CRLF that closes each part"),
    ("m14", M, '        let Some(buffered) = buffered else {',
     '        let Some(buffered) = None::<Vec<Bytes>>.or(buffered).filter(|_| false) else {',
     "encode: never hand back a Rewindable, always a single-pass Streaming"),
    ("m15", M, '                Segment::Stream(_) => None,',
     '                Segment::Stream(_) => Some(Bytes::new()),',
     "encode: call a form with a stream in it replayable (and lose the stream)"),
    ("m16", M, '        let mut remaining = Some(0u64);', '        let mut remaining: Option<u64> = None;',
     "size: never know the length, so every form is chunked"),
    ("m17", M, '                Segment::Stream(s) => s.size_hint().exact(),',
     '                Segment::Stream(s) => Some(s.size_hint().lower()),',
     "size: read the stream's lower bound instead of its exact length"),
    ("m18", M, '                        Err(_) => {\n                            return Poll::Ready(Some(Err(Error::new(\n                                ErrorKind::Body,\n                                TrailersInAPart,\n                            ))));\n                        }',
     '                        Err(_) => continue,',
     "poll_frame: drop a part's trailers frame instead of failing"),
    ("m19", M, '                    if b.is_empty() {\n                        continue;\n                    }', '                    {}',
     "poll_frame: emit an empty segment as a zero-length frame"),
    ("m20", M, '            this.remaining = this.remaining.map(|r| r.saturating_sub(data.len() as u64));',
     '            let _ = data.len();',
     "poll_frame: never decrement, so size_hint reports the total for ever"),
    ("m21", R, '            if headers.contains_key(http::header::CONTENT_TYPE) {\n                return Err(Error::new(ErrorKind::Other, ContentTypeIsNotOursToKeep));\n            }\n            headers.insert(http::header::CONTENT_TYPE, b.content_type());',
     '            headers.insert(http::header::CONTENT_TYPE, b.content_type());',
     "send: override a caller-set Content-Type instead of refusing"),
    ("m22", R, '        self.body = RequestBody::Full(bytes::Bytes::from(encoded));\n        self.multipart = None;',
     '        self.body = RequestBody::Full(bytes::Bytes::from(encoded));',
     "form(): leave the multipart mark set, so the boundary outlives its body"),
    ("m23", R, 'let boundary = match crate::multipart::Boundary::random() {',
     'let boundary = match crate::multipart::Boundary::new("fixed") {',
     "multipart(): one boundary for every request in the process"),
    # ---- CONTROLS ----
    ("c1", M, '#[non_exhaustive]\npub enum MultipartError {', 'pub enum MultipartError {',
     "CONTROL: remove #[non_exhaustive] — a no-op inside the defining crate"),
    ("c2", M, '        let mut segments = Vec::with_capacity(self.parts.len() * 3 + 1);',
     '        let mut segments = Vec::new();',
     "CONTROL: drop the capacity hint"),
]


def run(cmd):
    return subprocess.run(cmd, shell=True, cwd=ROOT, capture_output=True, text=True)


def main():
    only = sys.argv[1:] or None
    results = []
    for mid, path, find, repl, desc in MUTATIONS:
        if only and mid not in only:
            continue
        full = os.path.join(ROOT, path)
        src = open(full).read()
        if src.count(find) != 1:
            print(f"{mid}: PATTERN NOT UNIQUE ({src.count(find)}) — skipped")
            results.append((mid, "BROKEN", desc))
            continue
        open(full, "w").write(src.replace(find, repl))
        run(f"touch {path}")
        out = run("cargo nextest run -p hclient --all-features --no-fail-fast 2>&1")
        text = out.stdout + out.stderr
        if "error[E" in text or "error: could not compile" in text:
            verdict = "KILLED (compile)"
        elif " failed" in text and "0 failed" not in text.replace("0 failed", "X"):
            verdict = "KILLED"
        elif out.returncode != 0:
            verdict = "KILLED"
        else:
            verdict = "SURVIVED"
        # which tests died
        dead = sorted(
            {
                l.split()[-1]
                for l in text.splitlines()
                if "FAIL" in l and "Summary" not in l and "error" not in l
            }
        )
        # restore
        run(f"git checkout -- {path}")
        run(f"touch {path}")
        print(f"{mid}: {verdict} — {desc}")
        if dead:
            print("      " + ", ".join(d for d in dead if d)[:400])
        results.append((mid, verdict, desc))
    print("\n==== SUMMARY ====")
    for mid, v, d in results:
        print(f"{mid:4} {v:16} {d}")


if __name__ == "__main__":
    main()
