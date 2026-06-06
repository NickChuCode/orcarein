//! Hands-on demo of tool-call argument repair (v0.2 ③).
//!
//! Run: `cargo run -p orcarein-core --example repair_demo`
//! No API key needed — it calls the pure repair function directly so you can
//! watch each malformed `arguments` string get salvaged (or honestly rejected).

use orcarein_core::tool::parse_tool_arguments;

fn main() {
    let cases = [
        ("empty string", ""),
        ("clean json", r#"{"path":"Cargo.toml"}"#),
        (
            "markdown fenced",
            "```json\n{\"path\": \"src/main.rs\"}\n```",
        ),
        ("wrapped in prose", "Sure! Here: {\"cmd\": \"ls -la\"} ok?"),
        (
            "nested + brace in string",
            r#"pre {"a": {"b": 1}, "s": "has } brace"} post"#,
        ),
        ("truncated (unrepairable)", r#"{"path": "x""#),
        ("garbage (unrepairable)", "not json at all"),
    ];

    println!("{:<28} | result", "input case");
    println!("{}", "-".repeat(72));
    for (label, raw) in cases {
        match parse_tool_arguments(raw) {
            Ok(v) => println!("{label:<28} | OK    {v}"),
            Err(reason) => println!("{label:<28} | ERR   {reason}"),
        }
    }
}
