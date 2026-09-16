//! Offline, reversible compaction selected by ordinary o200k_base token counts.

mod json_codec;
mod text_codec;
mod text_refs;

use serde::{Deserialize, Serialize};
use tiktoken_rs::CoreBPE;

/// The explicit restoration format; raw text is never interpreted as framing.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Encoding {
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "json-v1")]
    JsonV1,
    #[serde(rename = "json-rows-v1")]
    JsonRowsV1,
    #[serde(rename = "text-runs-v1")]
    TextRunsV1,
    #[serde(rename = "text-prefixes-v1")]
    TextPrefixesV1,
    #[serde(rename = "text-refs-v1")]
    TextRefsV1,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompactResult {
    pub text: String,
    pub encoding: Encoding,
    pub input_tokens: usize,
    pub output_tokens: usize,
}

/// Reuse an instance to avoid loading the embedded tokenizer for each input.
pub struct Compactor {
    tokenizer: CoreBPE,
}

impl Compactor {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            tokenizer: tiktoken_rs::o200k_base()?,
        })
    }

    /// Choose only strictly cheaper complete representations. Ties favor raw,
    /// then the first codec candidate. Special-token-looking text is ordinary text.
    pub fn compact(&self, text: &str) -> CompactResult {
        let input_tokens = self.tokenizer.encode_ordinary(text).len();
        let mut result = CompactResult {
            text: text.to_owned(),
            encoding: Encoding::Raw,
            input_tokens,
            output_tokens: input_tokens,
        };
        let candidates = json_codec::candidates(text)
            .into_iter()
            .chain(text_codec::candidate(text).map(|candidate| (Encoding::TextRunsV1, candidate)))
            .chain(
                text_codec::prefix_candidate(text)
                    .map(|candidate| (Encoding::TextPrefixesV1, candidate)),
            )
            .chain(text_refs::candidate(text).map(|candidate| (Encoding::TextRefsV1, candidate)))
            .chain(
                text_refs::fragment_candidates(text)
                    .map(|candidate| (Encoding::TextRefsV1, candidate)),
            );
        for (encoding, candidate) in candidates {
            let tokens = self.tokenizer.encode_ordinary(&candidate).len();
            if tokens < result.output_tokens {
                result.text = candidate;
                result.encoding = encoding;
                result.output_tokens = tokens;
            }
        }
        result
    }
}

/// Restore a representation using its explicitly supplied format.
pub fn restore(encoding: Encoding, text: &str) -> anyhow::Result<String> {
    match encoding {
        Encoding::Raw => Ok(text.to_owned()),
        Encoding::TextRunsV1 => text_codec::restore(text),
        Encoding::TextPrefixesV1 => text_codec::restore_prefixes(text),
        Encoding::TextRefsV1 => text_refs::restore(text),
        Encoding::JsonV1 | Encoding::JsonRowsV1 => json_codec::restore(encoding, text),
    }
}
