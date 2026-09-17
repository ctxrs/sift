//! Offline, reversible compaction selected by ordinary o200k_base token counts.

mod json_codec;
mod text_codec;
mod text_refs;

use serde::{Deserialize, Serialize};

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

/// Instances share the embedded ordinary o200k tokenizer within the process.
pub struct Compactor {
    tokenizer: &'static bpe_openai::Tokenizer,
}

impl Compactor {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            tokenizer: bpe_openai::o200k_base(),
        })
    }

    /// Count one complete text using the same ordinary-token policy as compaction.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer.count(text)
    }

    /// Choose only strictly cheaper complete representations. Ties favor raw,
    /// then the first codec candidate. Special-token-looking text is ordinary text.
    pub fn compact(&self, text: &str) -> CompactResult {
        let input_tokens = self.count_tokens(text);
        let mut selected = None;
        let mut encoding = Encoding::Raw;
        let mut output_tokens = input_tokens;
        let candidates = json_codec::candidates(text)
            .into_iter()
            .chain(
                std::iter::once_with(|| {
                    text_codec::candidate(text).map(|candidate| (Encoding::TextRunsV1, candidate))
                })
                .flatten(),
            )
            .chain(
                std::iter::once_with(|| {
                    text_codec::prefix_candidate(text)
                        .map(|candidate| (Encoding::TextPrefixesV1, candidate))
                })
                .flatten(),
            )
            .chain(
                std::iter::once_with(|| {
                    text_refs::candidate(text).map(|candidate| (Encoding::TextRefsV1, candidate))
                })
                .flatten(),
            )
            .chain(
                text_refs::fragment_candidates(text)
                    .map(|candidate| (Encoding::TextRefsV1, candidate)),
            );
        for (candidate_encoding, candidate) in candidates {
            let tokens = self.count_tokens(&candidate);
            if tokens < output_tokens {
                selected = Some(candidate);
                encoding = candidate_encoding;
                output_tokens = tokens;
            }
        }
        CompactResult {
            text: selected.unwrap_or_else(|| text.to_owned()),
            encoding,
            input_tokens,
            output_tokens,
        }
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
