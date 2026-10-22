//! The JSON-lines protocol under malformed, oversized or hostile requests.

mod common;

use std::io::Cursor;

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::server::{serve, MAX_REQUEST_BYTES};
use serde_json::Value;

/// Run `serve` over `input`; the JSON response lines (binary payloads skipped).
fn run(input: Vec<u8>) -> Vec<Value> {
    let out = within(60, "serve", move || {
        let mut out = Vec::new();
        serve(Cursor::new(input), &mut out).unwrap();
        out
    });
    let mut replies = Vec::new();
    let mut rest = &out[..];
    while !rest.is_empty() {
        let end = rest.iter().position(|&b| b == b'\n').expect("a response line");
        let v: Value = serde_json::from_slice(&rest[..end]).unwrap();
        rest = &rest[end + 1..];
        if let Some(n) = v.get("bytes").and_then(Value::as_u64) {
            rest = &rest[n as usize..];
        }
        replies.push(v);
    }
    assert_eq!(replies[0]["event"], "hello");
    replies.remove(0);
    replies
}

fn lines(reqs: &[&str]) -> Vec<u8> {
    reqs.iter().flat_map(|r| format!("{r}\n").into_bytes()).collect()
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("ok")
}

#[test]
fn malformed_requests_get_errors_and_the_session_goes_on() {
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let replies = run(lines(&[
        "not json",
        "[]",
        "42",
        "null",
        r#"{"op": 3}"#,
        r#"{"op": "explode", "id": 1}"#,
        &deep,
        r#"{"id": 2, "op": "slice", "view": "axial", "index": 0}"#,
        r#"{"id": 3, "op": "value", "ijk": [1, 2]}"#,
        "",
        "   ",
        r#"{"id": 4, "op": "ping"}"#,
    ]));
    let codes: Vec<&str> = replies.iter().map(code).collect();
    assert_eq!(codes, ["bad_request"; 9].iter().copied().chain(["ok"]).collect::<Vec<_>>());
    assert_eq!(replies[5]["id"], 1);
    assert_eq!(replies.last().unwrap()["id"], 4);
}

#[test]
fn oversized_lines_are_skipped_without_losing_the_next_request() {
    let mut input = vec![b'x'; 8 * MAX_REQUEST_BYTES];
    input.push(b'\n');
    input.extend(lines(&[r#"{"id": 1, "op": "ping"}"#]));
    let mut exact = br#"{"id": 2, "op": "ping", "pad": ""#.to_vec();
    exact.resize(MAX_REQUEST_BYTES - 2, b' ');
    exact.extend(br#""}"#);
    assert_eq!(exact.len(), MAX_REQUEST_BYTES);
    input.extend(exact);
    input.push(b'\n');
    input.extend(vec![b'y'; MAX_REQUEST_BYTES + 10]); // no final newline: EOF
    let replies = run(input);
    let codes: Vec<&str> = replies.iter().map(code).collect();
    assert_eq!(codes, ["bad_request", "ok", "ok", "bad_request"]);
    assert_eq!(replies[1]["id"], 1);
    assert_eq!(replies[2]["id"], 2);
}

#[test]
fn out_of_range_indices_are_clamped_or_refused() {
    let dir = tmpdir("proto");
    let n = Nifti::new(&[5, 6, 7], 4);
    let data: Vec<u8> = (0..5 * 6 * 7 * 2).map(|i| (i % 50) as u8).collect();
    let p = dir.join("p.nii.gz");
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    let open = format!(r#"{{"id": 1, "op": "open", "path": {}}}"#, serde_json::json!(p.to_str().unwrap()));
    let replies = run(lines(&[
        &open,
        r#"{"id": 2, "op": "slice", "view": "axial", "index": 18446744073709551615}"#,
        r#"{"id": 3, "op": "slice", "view": "axial", "index": -9223372036854775808}"#,
        r#"{"id": 4, "op": "slice", "view": "axial", "index": 1e30}"#,
        r#"{"id": 5, "op": "value", "ijk": [-1, 0, 0]}"#,
        r#"{"id": 6, "op": "value", "ijk": [0, 0, 18446744073709551615]}"#,
        r#"{"id": 7, "op": "value", "ijk": [4, 5, 6]}"#,
        r#"{"id": 8, "op": "set_display", "mode": "stored", "colormap": "labels"}"#,
        r#"{"id": 9, "op": "close"}"#,
        r#"{"id": 10, "op": "ping"}"#,
    ]));
    let codes: Vec<&str> = replies.iter().map(code).collect();
    assert_eq!(codes, ["ok", "ok", "ok", "bad_request", "bad_request", "bad_request", "ok", "ok", "ok"]);
    assert_eq!(replies[1]["index"], 6);
    assert_eq!(replies[2]["index"], 0);
    assert_eq!(replies[1]["bytes"], 5 * 6 * 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn environment_limits_are_clamped() {
    use gnome_nifti_preview::volume::bytes_from_env;
    let range = (16 << 20, 64 << 30);
    for (value, expected) in [
        ("abc", 7),
        ("-1", 7),
        ("", 7),
        ("0", 16 << 20),
        ("99999999999999999999", 7),
        ("18446744073709551615", 64 << 30),
        (" 123456789 ", 123456789),
    ] {
        let name = format!("NP_TEST_BYTES_{}", value.len());
        std::env::set_var(&name, value);
        assert_eq!(bytes_from_env(&name, 7, range), expected, "{value:?}");
        std::env::remove_var(&name);
    }
    assert_eq!(bytes_from_env("NP_TEST_UNSET_VARIABLE", 7, range), 7);
}
