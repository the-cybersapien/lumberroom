//! Embedding. Implementations hold their own interior mutability: fastembed's `embed` takes
//! `&mut self` and is CPU-bound, so the adapter both locks and moves the work off the runtime.

use async_trait::async_trait;

use crate::domain::errors::Result;

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Recorded on every row so a model swap is detectable later.
    fn id(&self) -> String;
    fn dim(&self) -> usize;
    async fn embed_documents(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
    /// Asymmetric models prefix queries; symmetric ones ignore the distinction.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
}
