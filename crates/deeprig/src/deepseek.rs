//! DeepSeek V4 chat client.
//!
//! Chapter 6 keeps this as a plain async function in the binary crate.
//! Chapter 13 will move it into `deeprig-core` behind a `Provider` trait,
//! next to an OpenAI implementation — but introducing that abstraction now,
//! with only one backend, would be premature.

use anyhow::{bail, Context, Result};
use deeprig_core::Message;
use serde::Deserialize;
use serde_json::json;

/// DeepSeek's OpenAI-compatible chat completions endpoint.
const API_URL: &str = "https://api.deepseek.com/v1/chat/completions";

/// One assistant turn returned by [`chat`].
pub struct ChatReply {
    /// The assistant message — append this to the conversation.
    pub message: Message,
    /// DeepSeek V4's chain-of-thought, if the model returned any.
    ///
    /// This is shown to the user but NOT appended to the conversation: only
    /// `content` is part of the context sent back on the next turn.
    pub reasoning: Option<String>,
}

/// The slice of DeepSeek's response we model. `serde` ignores the rest.
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Sends the whole conversation to DeepSeek and returns one assistant turn.
///
/// `messages` is the full running conversation — DeepSeek is stateless, so
/// every turn re-sends the entire history.
pub async fn chat(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    messages: &[Message],
) -> Result<ChatReply> {
    let response = client
        .post(API_URL)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "messages": messages,
            "stream": false,
        }))
        .send()
        .await
        .context("HTTP request to DeepSeek failed")?;

    // Read status and body separately so a 4xx/5xx error keeps DeepSeek's
    // own explanation (the body) instead of discarding it.
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read the response body")?;
    if !status.is_success() {
        bail!("DeepSeek returned {status}:\n{body}");
    }

    parse_reply(&body)
}

/// Parses a successful DeepSeek response body into a [`ChatReply`].
///
/// Split out from [`chat`] so it can be unit-tested without the network.
fn parse_reply(body: &str) -> Result<ChatReply> {
    let parsed: ChatResponse =
        serde_json::from_str(body).context("response JSON did not match the expected shape")?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .context("DeepSeek returned an empty `choices` array")?;

    Ok(ChatReply {
        message: Message::assistant(choice.message.content),
        reasoning: choice.message.reasoning_content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_reply() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        let reply = parse_reply(body).expect("should parse");
        assert_eq!(reply.message.role, "assistant");
        assert_eq!(reply.message.content, "hi");
        assert!(reply.reasoning.is_none());
    }

    #[test]
    fn parses_a_reply_with_reasoning() {
        let body = r#"{"choices":[{"message":{"role":"assistant",
            "content":"42","reasoning_content":"first I thought..."}}]}"#;
        let reply = parse_reply(body).expect("should parse");
        assert_eq!(reply.message.content, "42");
        assert_eq!(reply.reasoning.as_deref(), Some("first I thought..."));
    }

    #[test]
    fn empty_choices_is_an_error() {
        let body = r#"{"choices":[]}"#;
        assert!(parse_reply(body).is_err());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_reply("not json").is_err());
    }
}
