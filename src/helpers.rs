use std::fmt::Display;
use std::io::{stdin, stdout, Write};

use log::{debug, error, info, trace, warn};

pub fn some_non_empty(str: &str) -> Option<String> {
    if str.is_empty() { None } else { Some(str.to_string()) }
}

// TODO as macro so that log comes from appropriate module
pub fn or_print<T, U: Display>(result: Result<T, U>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            warn!("{}", error);
            None
        }
    }
}

pub fn prompt(prompt: &str) -> Option<String> {
    print!("{} ", prompt);
    stdout().flush().unwrap();
    match stdin().lines().next() {
        Some(Ok(line)) => Some(line),
        _ => None,
    }
}

