//! Terminal IO behind a trait so wizard logic can be tested without a TTY.

use crate::Result;

/// Everything the wizard needs from the terminal.
pub trait Prompter {
    /// Pick one of `items`; returns its index.
    fn select(&mut self, prompt: &str, items: &[String], default: usize) -> Result<usize>;
    /// Free text input.
    fn input(&mut self, prompt: &str, default: Option<&str>) -> Result<String>;
    /// Hidden input (may be empty).
    fn password(&mut self, prompt: &str) -> Result<String>;
    /// Print a line.
    fn say(&mut self, msg: &str);
}

/// Real terminal implementation based on `dialoguer`.
pub struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn select(&mut self, prompt: &str, items: &[String], default: usize) -> Result<usize> {
        Ok(dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(default)
            .interact()?)
    }

    fn input(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        let mut i = dialoguer::Input::<String>::new().with_prompt(prompt);
        if let Some(d) = default {
            i = i.default(d.to_string());
        } else {
            i = i.allow_empty(true);
        }
        Ok(i.interact_text()?)
    }

    fn password(&mut self, prompt: &str) -> Result<String> {
        Ok(dialoguer::Password::new()
            .with_prompt(prompt)
            .allow_empty_password(true)
            .interact()?)
    }

    fn say(&mut self, msg: &str) {
        println!("{msg}");
    }
}

/// Ask for the encrypted-file passphrase (confirming it when creating a file).
#[must_use]
pub fn ask_passphrase(creating: bool) -> Option<String> {
    let mut p = dialoguer::Password::new().with_prompt(if creating {
        "New secrets-file passphrase"
    } else {
        "Secrets-file passphrase"
    });
    if creating {
        p = p.with_confirmation("Repeat passphrase", "Passphrases do not match");
    }
    p.interact().ok()
}

#[cfg(test)]
pub mod scripted {
    //! A scripted prompter for tests.

    use std::collections::VecDeque;

    use super::Prompter;
    use crate::Result;

    /// One scripted answer.
    #[derive(Debug, Clone)]
    pub enum Answer {
        Select(usize),
        Text(String),
    }

    /// Replays answers in order and records everything said.
    #[derive(Default)]
    pub struct Scripted {
        pub answers: VecDeque<Answer>,
        pub said: Vec<String>,
        pub prompts: Vec<String>,
    }

    impl Scripted {
        pub fn new(answers: Vec<Answer>) -> Self {
            Self {
                answers: answers.into(),
                ..Self::default()
            }
        }

        fn next(&mut self, prompt: &str) -> Result<Answer> {
            self.prompts.push(prompt.to_string());
            self.answers
                .pop_front()
                .ok_or_else(|| format!("script exhausted at prompt {prompt:?}").into())
        }
    }

    impl Prompter for Scripted {
        fn select(&mut self, prompt: &str, items: &[String], _default: usize) -> Result<usize> {
            match self.next(prompt)? {
                Answer::Select(i) if i < items.len() => Ok(i),
                a => Err(format!("bad answer {a:?} for select {prompt:?}").into()),
            }
        }

        fn input(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
            match self.next(prompt)? {
                Answer::Text(t) if t.is_empty() => Ok(default.unwrap_or_default().to_string()),
                Answer::Text(t) => Ok(t),
                a @ Answer::Select(_) => {
                    Err(format!("bad answer {a:?} for input {prompt:?}").into())
                }
            }
        }

        fn password(&mut self, prompt: &str) -> Result<String> {
            match self.next(prompt)? {
                Answer::Text(t) => Ok(t),
                a @ Answer::Select(_) => {
                    Err(format!("bad answer {a:?} for password {prompt:?}").into())
                }
            }
        }

        fn say(&mut self, msg: &str) {
            self.said.push(msg.to_string());
        }
    }
}
