//! Line protocol between the resident agent and its short-lived encoder process.
//!
//! The agent writes one escaped path per line to the worker's stdin; the worker answers each
//! with one escaped, tab-separated outcome line. Closing stdin makes the worker exit, which
//! returns all decoder/encoder memory to the OS.

use std::path::PathBuf;

use crate::config::OriginalPolicy;
use crate::convert::{Fingerprint, Outcome};

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn policy_str(p: OriginalPolicy) -> &'static str {
    match p {
        OriginalPolicy::Delete => "delete",
        OriginalPolicy::Trash => "trash",
        OriginalPolicy::Keep => "keep",
    }
}

fn fp_str(fp: Option<Fingerprint>) -> String {
    fp.map_or_else(|| "-".to_string(), |f| format!("{}:{}", f.size, f.mtime_ns))
}

fn parse_fp(s: &str) -> Option<Option<Fingerprint>> {
    if s == "-" {
        return Some(None);
    }
    let (size, mtime) = s.split_once(':')?;
    Some(Some(Fingerprint { size: size.parse().ok()?, mtime_ns: mtime.parse().ok()? }))
}

pub fn outcome_to_line(o: &Outcome) -> String {
    let e = |s: &str| escape(s);
    let p = |p: &PathBuf| escape(&p.to_string_lossy());
    match o {
        Outcome::Converted { output, before, after, original, note } => {
            format!(
                "converted\t{}\t{before}\t{after}\t{}\t{}",
                p(output),
                policy_str(*original),
                e(note.as_deref().unwrap_or(""))
            )
        }
        Outcome::WouldConvert(out) => format!("would\t{}", p(out)),
        Outcome::AlreadyDone(out) => format!("done\t{}", p(out)),
        Outcome::Skipped { reason, remember } => format!("skipped\t{}\t{}", fp_str(*remember), e(reason)),
        Outcome::Ignored => "ignored".to_string(),
        Outcome::Requeue(r) => format!("requeue\t{}", e(r)),
        Outcome::Failed { reason, remember } => format!("failed\t{}\t{}", fp_str(*remember), e(reason)),
    }
}

pub fn outcome_from_line(line: &str) -> Option<Outcome> {
    let f: Vec<String> = line.split('\t').map(unescape).collect();
    let get = |i: usize| f.get(i).cloned();
    Some(match f.first()?.as_str() {
        "converted" => Outcome::Converted {
            output: PathBuf::from(get(1)?),
            before: get(2)?.parse().ok()?,
            after: get(3)?.parse().ok()?,
            original: match get(4)?.as_str() {
                "delete" => OriginalPolicy::Delete,
                "trash" => OriginalPolicy::Trash,
                _ => OriginalPolicy::Keep,
            },
            note: get(5).filter(|n| !n.is_empty()),
        },
        "would" => Outcome::WouldConvert(PathBuf::from(get(1)?)),
        "done" => Outcome::AlreadyDone(PathBuf::from(get(1)?)),
        "skipped" => Outcome::Skipped { remember: parse_fp(&get(1)?)?, reason: get(2)? },
        "ignored" => Outcome::Ignored,
        "requeue" => Outcome::Requeue(get(1)?),
        "failed" => Outcome::Failed { remember: parse_fp(&get(1)?)?, reason: get(2)? },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_roundtrip() {
        for s in ["plain", "tab\there", "new\nline", "back\\slash\\t", "C:\\Users\\写真\\a.jpg", ""] {
            let e = escape(s);
            assert!(!e.contains('\t') && !e.contains('\n'));
            assert_eq!(unescape(&e), s);
        }
    }

    #[test]
    fn outcome_roundtrip() {
        let fp = Some(Fingerprint { size: 42, mtime_ns: -7 });
        let cases = vec![
            Outcome::Converted {
                output: PathBuf::from("/a\tb/c.avif"),
                before: 100,
                after: 40,
                original: OriginalPolicy::Trash,
                note: Some("x\ny".into()),
            },
            Outcome::Converted {
                output: PathBuf::from("c.avif"),
                before: 1,
                after: 0,
                original: OriginalPolicy::Delete,
                note: None,
            },
            Outcome::WouldConvert(PathBuf::from("w.avif")),
            Outcome::AlreadyDone(PathBuf::from("d.avif")),
            Outcome::Skipped { reason: "big".into(), remember: fp },
            Outcome::Skipped { reason: "".into(), remember: None },
            Outcome::Ignored,
            Outcome::Requeue("changed".into()),
            Outcome::Failed { reason: "bad\\data".into(), remember: fp },
        ];
        for o in cases {
            assert_eq!(outcome_from_line(&outcome_to_line(&o)).as_ref(), Some(&o));
        }
        assert_eq!(outcome_from_line("garbage"), None);
    }
}
