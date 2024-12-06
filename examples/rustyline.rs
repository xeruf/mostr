use rustyline::error::ReadlineError;
use rustyline::{Cmd, ConditionalEventHandler, DefaultEditor, Event, EventContext, EventHandler, KeyEvent, Movement, RepeatCount, Result};

struct CtrlCHandler;
impl ConditionalEventHandler for CtrlCHandler {
    fn handle(&self, evt: &Event, n: RepeatCount, positive: bool, ctx: &EventContext) -> Option<Cmd> {
        Some(if !ctx.line().is_empty() {
            Cmd::Kill(Movement::WholeLine)
        } else {
            Cmd::Interrupt
        })
    }
}

fn main() -> Result<()> {
    // `()` can be used when no completer is required
    let mut rl = DefaultEditor::new()?;

    rl.bind_sequence(
        KeyEvent::ctrl('c'),
        EventHandler::Conditional(Box::from(CtrlCHandler)));
    #[cfg(feature = "with-file-history")]
    if rl.load_history("history.txt").is_err() {
        println!("No previous history.");
    }
    loop {
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(line.as_str());
                println!("Line: {}", line);
            },
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                break
            },
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break
            },
            Err(err) => {
                println!("Error: {:?}", err);
                break
            }
        }
    }
    #[cfg(feature = "with-file-history")]
    rl.save_history("history.txt");
    Ok(())
}