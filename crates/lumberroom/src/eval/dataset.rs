//! Loading LongMemEval-S.
//!
//! The file is 265MB of JSON holding 500 questions and roughly 53 sessions each. Reading it whole
//! costs the memory it costs; the alternative is a streaming parser for a file read once per run.

use std::path::{Path, PathBuf};

use crate::client::{err, truncate, Result};
use crate::eval::Question;

/// Under this many questions the file is the wrong file. A subset somebody cut by hand is still
/// worth running; a download that stopped after three questions is a score nobody can compare.
const MIN_QUESTIONS: usize = 100;

/// Where the dataset sits unless `--dataset` names somewhere else.
pub fn default_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("LUMBERROOM_LME_DATASET") {
        if !path.trim().is_empty() {
            return Ok(PathBuf::from(path.trim()));
        }
    }
    Ok(crate::ingest::state_dir()?.join("eval").join("longmemeval_s.json"))
}

/// Parse the file. Refuses a file that is not the 500-question array, naming what it found, since
/// the wrong file silently producing three questions is worse than an error.
pub fn load(path: &Path) -> Result<Vec<Question>> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        err(format!(
            "cannot read the dataset at {}: {e}. Download longmemeval_s.json and point \
             LUMBERROOM_LME_DATASET or --dataset at it",
            path.display()
        ))
    })?;

    // The array check runs on the text, before serde. A file holding an object gets an error naming
    // what it starts with, rather than a type error thrown from somewhere 200MB in.
    let head = raw.trim_start();
    if !head.starts_with('[') {
        return Err(err(format!(
            "{} does not hold a JSON array of questions. It starts with: {}",
            path.display(),
            truncate(head, 80)
        )));
    }

    let questions: Vec<Question> = serde_json::from_str(&raw)
        .map_err(|e| err(format!("{} is not LongMemEval-S: {e}", path.display())))?;

    if questions.len() < MIN_QUESTIONS {
        return Err(err(format!(
            "{} holds {} questions and LongMemEval-S holds 500. A partial file scores a partial \
             corpus and the number would not mean anything",
            path.display(),
            questions.len()
        )));
    }
    Ok(questions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lumberroom-eval-{name}-{}.json", uuid::Uuid::new_v4()))
    }

    fn question_json(index: usize) -> String {
        format!(
            r#"{{"question_id":"q{index}","question_type":"multi-session","question":"who",
               "question_date":"2023/05/30 (Tue) 23:40","haystack_session_ids":["s{index}"],
               "haystack_sessions":[[{{"role":"user","content":"hello","has_answer":true}}]],
               "haystack_dates":["2023/05/20 (Sat) 02:21"],"answer_session_ids":["s{index}"]}}"#
        )
    }

    fn array_of(n: usize) -> String {
        let rows: Vec<String> = (0..n).map(question_json).collect();
        format!("[{}]", rows.join(","))
    }

    #[test]
    fn a_full_array_parses_and_ignores_fields_this_client_does_not_read() {
        let path = scratch("full");
        std::fs::write(&path, array_of(100)).unwrap();
        let questions = load(&path).unwrap();
        assert_eq!(questions.len(), 100);
        assert_eq!(questions[0].haystack_sessions[0][0].role, "user");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_short_array_is_refused_and_the_error_says_how_short() {
        let path = scratch("short");
        std::fs::write(&path, array_of(3)).unwrap();
        let e = load(&path).unwrap_err();
        assert!(e.message.contains("holds 3 questions"), "{}", e.message);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_an_array_is_refused_before_serde_sees_it() {
        let path = scratch("object");
        std::fs::write(&path, r#"{"questions": []}"#).unwrap();
        let e = load(&path).unwrap_err();
        assert!(e.message.contains("does not hold a JSON array"), "{}", e.message);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_names_the_path_and_the_override() {
        let e = load(Path::new("/nonexistent/longmemeval_s.json")).unwrap_err();
        assert!(e.message.contains("LUMBERROOM_LME_DATASET"), "{}", e.message);
    }
}
