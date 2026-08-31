use serde_json::Value;
use thiserror::Error;

/// Wire framing used by a provider's streaming response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamProtocol {
    /// OpenAI/OpenRouter/compatible server-sent events.
    OpenAi,
    /// Anthropic typed server-sent events.
    Anthropic,
    /// Gemini JSON objects delivered as server-sent events.
    Gemini,
    /// Ollama newline-delimited JSON.
    Ollama,
}

/// Normalized streaming output consumed by the palette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamEvent {
    /// Text appended to the current assistant message.
    Text(String),
    /// Provider completed normally.
    Done,
}

/// Incremental decoder retaining partial transport lines between reads.
#[derive(Debug)]
pub struct StreamDecoder {
    protocol: StreamProtocol,
    pending: String,
}

impl StreamDecoder {
    /// Create a decoder for one provider protocol.
    #[must_use]
    pub const fn new(protocol: StreamProtocol) -> Self {
        Self {
            protocol,
            pending: String::new(),
        }
    }

    /// Consume an arbitrary UTF-8 transport fragment.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError`] for malformed JSON or a provider error payload.
    pub fn push(&mut self, fragment: &str) -> Result<Vec<StreamEvent>, StreamError> {
        self.pending.push_str(fragment);
        let mut events = Vec::new();
        while let Some(position) = self.pending.find('\n') {
            let mut line = self.pending.drain(..=position).collect::<String>();
            line.truncate(line.trim_end_matches(['\r', '\n']).len());
            if let Some(event) = decode_line(self.protocol, &line)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    /// Reject a non-empty truncated line when the response stream closes.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Truncated`] if an incomplete event remains.
    pub fn finish(self) -> Result<(), StreamError> {
        if self.pending.trim().is_empty() {
            Ok(())
        } else {
            Err(StreamError::Truncated)
        }
    }
}

/// Streaming protocol failures without logging prompt or response contents.
#[derive(Debug, Error)]
pub enum StreamError {
    /// Provider returned malformed JSON.
    #[error("AI provider returned malformed streaming data")]
    Json(#[from] serde_json::Error),
    /// Provider returned an explicit error object.
    #[error("AI provider reported a request failure")]
    Provider,
    /// Transport closed partway through an event.
    #[error("AI provider stream ended unexpectedly")]
    Truncated,
}

fn decode_line(protocol: StreamProtocol, line: &str) -> Result<Option<StreamEvent>, StreamError> {
    let payload = match protocol {
        StreamProtocol::Ollama => line.trim(),
        _ => line.strip_prefix("data:").map_or("", str::trim),
    };
    if payload.is_empty() {
        return Ok(None);
    }
    if payload == "[DONE]" {
        return Ok(Some(StreamEvent::Done));
    }
    let value: Value = serde_json::from_str(payload)?;
    if value.get("error").is_some() || value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(StreamError::Provider);
    }
    Ok(match protocol {
        StreamProtocol::OpenAi => decode_openai(&value),
        StreamProtocol::Anthropic => decode_anthropic(&value),
        StreamProtocol::Gemini => decode_gemini(&value),
        StreamProtocol::Ollama => decode_ollama(&value),
    })
}

fn decode_openai(value: &Value) -> Option<StreamEvent> {
    let choice = value.get("choices")?.as_array()?.first()?;
    choice
        .pointer("/delta/content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(|text| StreamEvent::Text(text.into()))
        .or_else(|| (!choice.get("finish_reason")?.is_null()).then_some(StreamEvent::Done))
}

fn decode_anthropic(value: &Value) -> Option<StreamEvent> {
    match value.get("type")?.as_str()? {
        "content_block_delta" => value
            .pointer("/delta/text")
            .and_then(Value::as_str)
            .map(|text| StreamEvent::Text(text.into())),
        "message_stop" => Some(StreamEvent::Done),
        _ => None,
    }
}

fn decode_gemini(value: &Value) -> Option<StreamEvent> {
    value
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .map(|text| StreamEvent::Text(text.into()))
        .or_else(|| {
            value
                .pointer("/candidates/0/finishReason")
                .is_some()
                .then_some(StreamEvent::Done)
        })
}

fn decode_ollama(value: &Value) -> Option<StreamEvent> {
    value
        .pointer("/message/content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(|text| StreamEvent::Text(text.into()))
        .or_else(|| {
            value
                .get("done")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .then_some(StreamEvent::Done)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_sse_survives_fragment_boundaries() {
        let mut decoder = StreamDecoder::new(StreamProtocol::OpenAi);
        assert!(decoder.push("data: {\"cho").expect("fragment").is_empty());
        assert_eq!(
            decoder
                .push("ices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n")
                .expect("events"),
            [StreamEvent::Text("Hi".into()), StreamEvent::Done]
        );
        decoder.finish().expect("clean finish");
    }

    #[test]
    fn normalizes_anthropic_gemini_and_ollama() {
        let cases = [
            (
                StreamProtocol::Anthropic,
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"A\"}}\n",
            ),
            (
                StreamProtocol::Gemini,
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"A\"}]}}]}\n",
            ),
            (
                StreamProtocol::Ollama,
                "{\"message\":{\"content\":\"A\"},\"done\":false}\n",
            ),
        ];
        for (protocol, input) in cases {
            assert_eq!(
                StreamDecoder::new(protocol).push(input).expect("event"),
                [StreamEvent::Text("A".into())]
            );
        }
    }

    #[test]
    fn errors_are_redacted_and_truncated_lines_fail_closed() {
        let mut decoder = StreamDecoder::new(StreamProtocol::OpenAi);
        assert!(matches!(
            decoder.push("data: {\"error\":{\"message\":\"secret\"}}\n"),
            Err(StreamError::Provider)
        ));
        let mut truncated = StreamDecoder::new(StreamProtocol::Ollama);
        truncated.push("{\"done\":").expect("buffer only");
        assert!(matches!(truncated.finish(), Err(StreamError::Truncated)));
    }
}
