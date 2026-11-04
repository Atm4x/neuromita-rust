//! Infrastructure adapters for external provider protocols.
pub mod fish_audio;
pub mod gemini_embedding;
pub(crate) mod pcm16;
pub mod python_embedding;
pub mod python_reranker;
pub(crate) mod speech_voice;
pub(crate) mod telegram_voice;

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod memory_backup;
