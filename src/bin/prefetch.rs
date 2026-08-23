//! Downloads the embedding weights at image build time.
//!
//! A first-request download on the VM is a silent failure mode when egress is locked down, so the
//! weights ship inside the image instead. Same reasoning as the previous build; different runtime.

fn main() {
    let provider = std::env::var("EMBED_PROVIDER").unwrap_or_else(|_| "local".into());
    if provider != "local" {
        println!("prefetch skipped: EMBED_PROVIDER={provider}");
        return;
    }

    let model = std::env::var("EMBED_MODEL").unwrap_or_else(|_| "Xenova/bge-base-en-v1.5".into());
    let cache = std::env::var("MODEL_CACHE_DIR").unwrap_or_else(|_| "/models".into());

    let started = std::time::Instant::now();
    let opts = fastembed::InitOptions::new(fastembed::EmbeddingModel::BGEBaseENV15Q)
        .with_cache_dir(cache.clone().into())
        .with_show_download_progress(false);

    let mut embedder = match fastembed::TextEmbedding::try_new(opts) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("prefetch failed: {e}");
            std::process::exit(1);
        }
    };

    match embedder.embed(vec!["prefetch"], None) {
        Ok(v) => println!(
            "prefetched {model} into {cache}: {} dims in {}ms",
            v[0].len(),
            started.elapsed().as_millis()
        ),
        Err(e) => {
            eprintln!("prefetch produced no vector: {e}");
            std::process::exit(1);
        }
    }
}
