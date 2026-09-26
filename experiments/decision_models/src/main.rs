//! Offline resident checkpoint probe. No tool dispatch or financial authority.
use scirust_sciagent::bpe::BpeTokenizer;
use scirust_sciagent::generate::Generator;
use scirust_sciagent::model::SciAgentModel;
use scirust_sciagent::train::checkpoint::{load_checkpoint, read_meta};
use serde_json::json;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("checkpoint required")?);
    let started = Instant::now();
    let meta = read_meta(&path)?;
    let mut model = SciAgentModel::new(&meta.config);
    load_checkpoint(&mut model, &path)?;
    let tokenizer = BpeTokenizer::from_embedded()?;
    let generator = Generator::new(&meta.config)
        .with_temperature(0.0)
        .with_repetition_penalty(1.3);
    println!(
        "{}",
        json!({"kind":"ready", "load_ms":started.elapsed().as_secs_f64()*1000.0,
        "max_seq_len":meta.config.max_seq_len,"max_new_tokens":8,"seed":42})
    );
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.len() > 8192 {
            return Err("request too large".into());
        }
        let request: serde_json::Value = serde_json::from_str(&line)?;
        let prompt = request["prompt"].as_str().ok_or("prompt required")?;
        let started = Instant::now();
        let ids = tokenizer.encode_with_special(prompt, true, false);
        if ids.len() + 8 > meta.config.max_seq_len {
            println!(
                "{}",
                json!({"error":"context_budget", "prompt_tokens":ids.len()})
            );
            continue;
        }
        let output = generator.generate(&mut model, &ids, 8, 42);
        let continuation = output.get(ids.len()..).ok_or("missing continuation")?;
        let response = tokenizer.decode(continuation);
        println!(
            "{}",
            json!({"response":response,"prompt_tokens":ids.len(),
            "generated_tokens":continuation.len(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0})
        );
    }
    Ok(())
}
